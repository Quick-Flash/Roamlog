//! GitHub prerelease selection and bounded, checksum-verified APK downloads.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
const REPO: &str = "https://github.com/Quick-Flash/Roamlog";
const MAX_APK: u64 = 256 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub code: u64,
    pub tag: String,
    pub notes: String,
    pub url: String,
    pub digest: String,
    pub size: u64,
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}
fn agent(seconds: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(seconds)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .https_only(true)
        .build()
        .into()
}
pub fn latest(current: u64, demo: bool) -> Result<Option<Update>, String> {
    let mut reply = agent(30)
        .get("https://api.github.com/repos/Quick-Flash/Roamlog/releases?per_page=100")
        .header("User-Agent", "Roamlog-updater")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let bytes = reply
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    select(&bytes, current, demo)
}
fn select(bytes: &[u8], current: u64, demo: bool) -> Result<Option<Update>, String> {
    let releases: Vec<Release> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let name = if demo {
        "roamlog-demo.apk"
    } else {
        "roamlog.apk"
    };
    Ok(releases
        .into_iter()
        .filter_map(|r| {
            if r.draft || !r.prerelease {
                return None;
            }
            let mut parts = r.tag_name.split('-');
            if parts.next() != Some("nightly") {
                return None;
            }
            let code = parts.next()?.parse::<u64>().ok()?;
            if code <= current
                || code > 2_100_000_000
                || !r
                    .tag_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return None;
            }
            let asset = r
                .assets
                .into_iter()
                .find(|a| a.name == name && a.state == "uploaded")?;
            let digest = asset.digest?.strip_prefix("sha256:")?.to_ascii_lowercase();
            if digest.len() != 64
                || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                || asset.size == 0
                || asset.size > MAX_APK
                || asset.browser_download_url
                    != format!("{REPO}/releases/download/{}/{name}", r.tag_name)
            {
                return None;
            }
            Some(Update {
                code,
                tag: r.tag_name,
                notes: r.body.unwrap_or_default(),
                url: asset.browser_download_url,
                digest,
                size: asset.size,
            })
        })
        .max_by_key(|u| u.code))
}
fn copy_verified(
    mut reader: impl Read,
    mut writer: impl Write,
    update: &Update,
) -> Result<(), String> {
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > update.size || count > MAX_APK {
            return Err("APK download exceeds expected size".into());
        }
        hash.update(&buf[..n]);
        writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    if count != update.size || format!("{:x}", hash.finalize()) != update.digest {
        return Err("APK checksum/size mismatch; update discarded".into());
    }
    Ok(())
}
pub fn download(update: &Update, directory: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let pending = directory.join("roamlog-update.apk.part");
    let final_path = directory.join("roamlog-update.apk");
    let result = (|| {
        let mut reply = agent(600)
            .get(&update.url)
            .header("User-Agent", "Roamlog-updater")
            .call()
            .map_err(|e| e.to_string())?;
        let mut file = fs::File::create(&pending).map_err(|e| e.to_string())?;
        copy_verified(reply.body_mut().as_reader(), &mut file, update)?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&pending, &final_path).map_err(|e| e.to_string())?;
        Ok(final_path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(pending);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn release(code: u64) -> serde_json::Value {
        let tag = format!("nightly-{code}-123-1");
        json!({"tag_name":tag,"draft":false,"prerelease":true,"body":"Changes", "assets":[
            {"name":"roamlog.apk","state":"uploaded","browser_download_url":format!("{REPO}/releases/download/{tag}/roamlog.apk"),"digest":format!("sha256:{}","a".repeat(64)),"size":3},
            {"name":"roamlog-demo.apk","state":"uploaded","browser_download_url":format!("{REPO}/releases/download/{tag}/roamlog-demo.apk"),"digest":format!("sha256:{}","b".repeat(64)),"size":3}]})
    }
    fn choose(values: Vec<serde_json::Value>, current: u64, demo: bool) -> Option<Update> {
        select(&serde_json::to_vec(&values).unwrap(), current, demo).unwrap()
    }
    #[test]
    fn selects_newest_version_despite_completion_order_and_isolates_demo() {
        let found = choose(
            vec![release(1002), release(1005), release(1003)],
            1001,
            false,
        )
        .unwrap();
        assert_eq!(found.code, 1005);
        assert!(found.url.ends_with("/roamlog.apk"));
        assert!(choose(vec![release(1005)], 1005, false).is_none());
        assert!(
            choose(vec![release(1005)], 1000, true)
                .unwrap()
                .url
                .ends_with("/roamlog-demo.apk")
        );
    }
    #[test]
    fn skips_drafts_missing_digests_bad_urls_and_incomplete_assets() {
        let mut draft = release(1009);
        draft["draft"] = json!(true);
        let mut digest = release(1008);
        digest["assets"][0]["digest"] = json!(null);
        let mut url = release(1007);
        url["assets"][0]["browser_download_url"] = json!("https://evil.example/roamlog.apk");
        let mut incomplete = release(1006);
        incomplete["assets"][0]["state"] = json!("new");
        assert_eq!(
            choose(
                vec![draft, digest, url, incomplete, release(1002)],
                1000,
                false
            )
            .unwrap()
            .code,
            1002
        );
    }
    #[test]
    fn corrupt_truncated_and_oversized_downloads_never_validate() {
        let mut update = choose(vec![release(1002)], 1000, false).unwrap();
        update.digest = format!("{:x}", Sha256::digest(b"apk"));
        assert!(copy_verified(&b"apk"[..], Vec::new(), &update).is_ok());
        for bytes in [&b"bad"[..], &b"ap"[..], &b"apk-extra"[..]] {
            assert!(copy_verified(bytes, Vec::new(), &update).is_err());
        }
    }
}
