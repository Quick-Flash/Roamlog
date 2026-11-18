//! Terrain-projected location overlays; pictures and labels match 2D map.
use super::{
    camera::Camera,
    terrain::{Orbit, Scene},
    view::FriendPinProps,
};
use crate::ui::state::UiState;
use std::sync::Arc;
use zgui::prelude::*;

#[component]
pub fn TerrainMarkers(
    scene: Signal<Arc<Scene>, LocalStorage>,
    camera: RwSignal<Camera, LocalStorage>,
    orbit: RwSignal<Orbit, LocalStorage>,
    size: Signal<[f64; 2], LocalStorage>,
) -> impl IntoView {
    let s = UiState::current();
    let marker = Memo::new(move |_| {
        let (point, _) = s.position.get()?;
        scene.with(|scene| scene.screen_position(point, camera.get(), orbit.get(), size.get()))
    });
    let friend_marks = Memo::new(move |_| {
        if !s.show_friends.get() {
            return Vec::new();
        }
        let camera = camera.get();
        let orbit = orbit.get();
        let size = size.get();
        let now = s.now.get() as i64;
        scene.with(|scene| {
            s.friend_markers.with(|markers| {
                markers
                    .iter()
                    .filter_map(|friend| {
                        let [x, y] = scene.screen_position(friend.point, camera, orbit, size)?;
                        let age = (now - friend.timestamp_ms).max(0) / 1000;
                        let when = match age {
                            0..60 => "now".to_owned(),
                            60..3600 => format!("{} min ago", age / 60),
                            _ => format!("{} h ago", age / 3600),
                        };
                        let label = match &friend.via {
                            Some(via) => format!("{} · via {via}, {when}", friend.name),
                            None => format!("{} · {when}", friend.name),
                        };
                        Some((
                            friend.id.clone(),
                            friend.name.clone(),
                            x,
                            y,
                            label,
                            friend.live && age < 900,
                            friend.avatar.clone(),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
            for friend in move ||friend_marks.get(),key=|f|(f.0.clone(),f.2.to_bits(),f.3.to_bits(),f.4.clone(),f.5,f.6.clone()) {
                FriendPin(id=friend.0.clone(),name=friend.1.clone(),x=friend.2,y=friend.3,label=friend.4.clone(),live=friend.5,avatar=friend.6.clone())
            }
            if move ||marker.get().is_some() {
                if move ||s.own_avatar.get().is_none() {
                    box(class="gps-dot",style:transform=move ||marker.get().map(|p|format!("translate({}px, {}px)",p[0]-8.,p[1]-8.))) {}
                }
                if move ||s.own_avatar.get().is_some() {
                    box(class="friend-avatar own-avatar friend-pin",style:transform=move ||marker.get().map(|p|format!("translate({}px, {}px)",p[0]-18.,p[1]-18.))) {
                        {zgui::elements::image().class("friend-avatar-image").src(Signal::derive_local(move ||s.own_avatar.get())).alt(Some("You".to_string())).into_view()}
                    }
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geo::Point,
        mapping::{
            camera::TileId,
            terrain::Tile,
            tiles::{Elevation, Payload, Provider},
        },
        ui::state::FriendMarker,
    };

    fn nodes_with_class(window: &zgui_testkit_view::Window, class: &str) -> Vec<NodeId> {
        let mut pending = vec![window.root];
        let mut found = Vec::new();
        while let Some(node) = pending.pop() {
            if window
                .dom
                .tree()
                .classes(node)
                .iter()
                .any(|c| c.as_str() == class)
            {
                found.push(node);
            }
            pending.extend(window.dom.tree().children(node));
        }
        found
    }

    #[test]
    fn terrain_shows_friend_photos_without_own_gps_and_tracks_visibility() {
        let directory = tempfile::tempdir().unwrap();
        let window = zgui_testkit_view::Window::open();
        let (s, scene, orbit) = window.scope.with(|| {
            let s = UiState::new(directory.path().to_owned());
            provide_local_context(s);
            s.camera.set(Camera {
                zoom: 12.,
                ..Default::default()
            });
            s.position.set(None);
            s.show_friends.set(true);
            s.now.set(120_000);
            s.friend_markers.set(vec![FriendMarker {
                id: "friend".into(),
                name: "Sam".into(),
                via: Some("Alex".into()),
                avatar: Some("sam.png".into()),
                point: Point {
                    lon: 0.001,
                    lat: 0.,
                },
                accuracy_m: 5.,
                timestamp_ms: 60_000,
                live: true,
            }]);
            let id = TileId { z: 0, x: 0, y: 0 };
            let scene = Scene::assembled(
                vec![Tile {
                    id,
                    dem_id: id,
                    raster_id: id,
                    provider: Provider::OpenStreetMap,
                    raster: None,
                    elevation: Some(Arc::new(Payload::Elevation(Elevation {
                        version: 1,
                        heights: vec![0.; 4],
                        width: 2,
                        height: 2,
                    }))),
                }],
                &Scene::default(),
                s.camera.get_untracked(),
            );
            (
                s,
                RwSignal::new_local(Arc::new(scene)),
                RwSignal::new_local(Orbit::default()),
            )
        });
        let mut mounted = window.scope.with(|| view! {
            TerrainMarkers(scene=Signal::derive_local(move ||scene.get()),camera=s.camera,orbit=orbit,size=Signal::derive_local(||[800.,600.]))
        }.into_view().build(&mut window.cx.cx()));
        mounted.mount(&window.dom_handle, window.root, None);
        window.frame();
        assert_eq!(nodes_with_class(&window, "friend-avatar").len(), 1);
        let label = nodes_with_class(&window, "friend-label")[0];
        assert_eq!(
            window.dom.tree().text_content(label),
            "Sam · via Alex, 1 min ago"
        );
        s.position.set(Some((Point { lon: 0., lat: 0. }, 5.)));
        s.own_avatar.set(Some("own.png".into()));
        window.frame();
        assert_eq!(nodes_with_class(&window, "friend-avatar").len(), 2);
        assert_eq!(nodes_with_class(&window, "own-avatar").len(), 1);
        s.now.set(1_000_000);
        window.frame();
        assert!(!nodes_with_class(&window, "friend-stale").is_empty());
        s.show_friends.set(false);
        window.frame();
        assert!(nodes_with_class(&window, "friend-label").is_empty());
        assert_eq!(nodes_with_class(&window, "own-avatar").len(), 1);
        s.own_avatar.set(None);
        window.frame();
        assert_eq!(nodes_with_class(&window, "gps-dot").len(), 1);
    }
}
