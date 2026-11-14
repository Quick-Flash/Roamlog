//! Minimal Mapbox Vector Tile 2.1 decoder: <https://github.com/mapbox/vector-tile-spec/tree/master/2.1>.
//! Reads layers, string/number/bool properties, and geometry commands.

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

#[derive(Debug)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    keys: Vec<String>,
    values: Vec<Value>,
    pub features: Vec<Feature>,
}

#[derive(Debug)]
pub struct Feature {
    /// 1 point, 2 line, 3 polygon.
    pub kind: u64,
    tags: Vec<u32>,
    geometry: Vec<u32>,
}

impl Layer {
    pub fn get<'a>(&'a self, feature: &Feature, key: &str) -> Option<&'a Value> {
        feature.tags.chunks_exact(2).find_map(|kv| {
            (self.keys.get(kv[0] as usize)? == key)
                .then(|| self.values.get(kv[1] as usize))
                .flatten()
        })
    }

    pub fn str<'a>(&'a self, feature: &Feature, key: &str) -> &'a str {
        match self.get(feature, key) {
            Some(Value::Str(s)) => s,
            _ => "",
        }
    }
}

/// Geometry step in tile units (0..extent, y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Move(f64, f64),
    Line(f64, f64),
    Close,
}

impl Feature {
    /// Walks geometry commands. Lines stay open; polygon rings end with Close.
    pub fn steps(&self, mut f: impl FnMut(Step)) {
        let (mut x, mut y) = (0_i64, 0_i64);
        let mut i = 0;
        let g = &self.geometry;
        while i < g.len() {
            let (command, count) = (g[i] & 7, g[i] >> 3);
            i += 1;
            if command == 7 {
                f(Step::Close);
                continue;
            }
            for _ in 0..count {
                let (Some(&dx), Some(&dy)) = (g.get(i), g.get(i + 1)) else {
                    return;
                };
                i += 2;
                x += zigzag(dx);
                y += zigzag(dy);
                f(if command == 1 {
                    Step::Move(x as f64, y as f64)
                } else {
                    Step::Line(x as f64, y as f64)
                });
            }
        }
    }
}

fn zigzag(v: u32) -> i64 {
    i64::from(v >> 1) ^ -i64::from(v & 1)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.pos).ok_or("Tile truncated")?;
            self.pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err("Tile varint overflow".into())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len());
        let end = end.ok_or("Tile truncated")?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Field number and wire type.
    fn key(&mut self) -> Result<(u64, u64), String> {
        let key = self.varint()?;
        Ok((key >> 3, key & 7))
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.varint()? as usize;
        self.take(n)
    }

    fn skip(&mut self, wire: u64) -> Result<(), String> {
        match wire {
            0 => self.varint().map(drop),
            1 => self.take(8).map(drop),
            2 => self.bytes().map(drop),
            5 => self.take(4).map(drop),
            _ => Err("Tile wire type unsupported".into()),
        }
    }

    fn packed(&mut self) -> Result<Vec<u32>, String> {
        let mut inner = Reader {
            bytes: self.bytes()?,
            pos: 0,
        };
        let mut out = Vec::new();
        while !inner.done() {
            out.push(inner.varint()? as u32);
        }
        Ok(out)
    }
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Layer>, String> {
    let mut r = Reader { bytes, pos: 0 };
    let mut layers = Vec::new();
    while !r.done() {
        match r.key()? {
            (3, 2) => layers.push(layer(r.bytes()?)?),
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(layers)
}

fn layer(bytes: &[u8]) -> Result<Layer, String> {
    let mut r = Reader { bytes, pos: 0 };
    let mut layer = Layer {
        name: String::new(),
        extent: 4096,
        keys: Vec::new(),
        values: Vec::new(),
        features: Vec::new(),
    };
    while !r.done() {
        match r.key()? {
            (1, 2) => layer.name = String::from_utf8_lossy(r.bytes()?).into(),
            (2, 2) => layer.features.push(feature(r.bytes()?)?),
            (3, 2) => layer.keys.push(String::from_utf8_lossy(r.bytes()?).into()),
            (4, 2) => layer.values.push(value(r.bytes()?)?),
            (5, 0) => layer.extent = r.varint()? as u32,
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(layer)
}

fn feature(bytes: &[u8]) -> Result<Feature, String> {
    let mut r = Reader { bytes, pos: 0 };
    let mut f = Feature {
        kind: 0,
        tags: Vec::new(),
        geometry: Vec::new(),
    };
    while !r.done() {
        match r.key()? {
            (2, 2) => f.tags = r.packed()?,
            (3, 0) => f.kind = r.varint()?,
            (4, 2) => f.geometry = r.packed()?,
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(f)
}

fn value(bytes: &[u8]) -> Result<Value, String> {
    let mut r = Reader { bytes, pos: 0 };
    let mut v = Value::Bool(false);
    while !r.done() {
        v = match r.key()? {
            (1, 2) => Value::Str(String::from_utf8_lossy(r.bytes()?).into()),
            (2, 5) => Value::Num(f64::from(f32::from_le_bytes(
                r.take(4)?.try_into().unwrap(),
            ))),
            (3, 1) => Value::Num(f64::from_le_bytes(r.take(8)?.try_into().unwrap())),
            (4 | 5, 0) => Value::Num(r.varint()? as i64 as f64),
            (6, 0) => {
                let n = r.varint()?;
                Value::Num(((n >> 1) as i64 ^ -((n & 1) as i64)) as f64)
            }
            (7, 0) => Value::Bool(r.varint()? != 0),
            (_, wire) => {
                r.skip(wire)?;
                continue;
            }
        };
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_layer_properties_and_geometry() {
        // Layer "roads", extent 4096, key "kind", value "path",
        // line feature: MoveTo(2,2) LineTo(+3,+4).
        let geometry = [9, 4, 4, 10, 6, 8];
        let mut feature = vec![0x12, 2, 0, 0, 0x18, 2, 0x22, geometry.len() as u8];
        feature.extend(geometry);
        let value = [0x0a, 4, b'p', b'a', b't', b'h'];
        let mut layer = vec![0x0a, 5, b'r', b'o', b'a', b'd', b's'];
        layer.extend([0x12, feature.len() as u8]);
        layer.extend(&feature);
        layer.extend([0x1a, 4, b'k', b'i', b'n', b'd']);
        layer.extend([0x22, value.len() as u8]);
        layer.extend(value);
        layer.extend([0x28, 0x80, 0x20]);
        let mut tile = vec![0x1a, layer.len() as u8];
        tile.extend(layer);
        let layers = decode(&tile).unwrap();
        let (l, f) = (&layers[0], &layers[0].features[0]);
        assert_eq!((l.name.as_str(), l.extent, f.kind), ("roads", 4096, 2));
        assert_eq!(l.str(f, "kind"), "path");
        let mut steps = Vec::new();
        f.steps(|s| steps.push(s));
        assert_eq!(steps, [Step::Move(2.0, 2.0), Step::Line(5.0, 6.0)]);
    }
}
