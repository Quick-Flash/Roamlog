//! Pairing controls used by standalone Android prototype.
use super::{
    Result,
    pairing::{self, FriendInvite},
    peers::{Friends, Permission},
};
use iroh::{Endpoint, EndpointId};
use jni::{
    Env, jni_sig, jni_str,
    objects::{JObject, JString},
};
use tokio::sync::watch;

pub(super) fn store(
    env: &mut Env<'_>,
    context: &JObject<'_>,
    owner: EndpointId,
) -> Result<Friends> {
    let directory = env
        .call_method(
            context,
            jni_str!("getNoBackupFilesDir"),
            jni_sig!("()Ljava/io/File;"),
            &[],
        )?
        .l()?;
    let path = env
        .call_method(
            &directory,
            jni_str!("getAbsolutePath"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let path = env.cast_local::<JString>(path)?.try_to_string(env)?;
    Friends::open(path, owner)
}

pub(super) fn local(mode: &str, target: &str, friends: &Friends) -> Result<bool> {
    match mode {
        "identity" | "friends" => {}
        "permission_off" => friends.set_permission(target.parse()?, Permission::Off)?,
        "permission_trips" => friends.set_permission(target.parse()?, Permission::TripsOnly)?,
        "permission_always" => friends.set_permission(target.parse()?, Permission::Always)?,
        "block" => friends.block(target.parse()?)?,
        "remove" => friends.remove(target.parse()?)?,
        _ => return Ok(false),
    }
    Ok(true)
}

pub(super) async fn pair(
    endpoint: &Endpoint,
    name: &str,
    invite: Option<FriendInvite>,
    friends: &Friends,
    votes: &mut watch::Receiver<Option<bool>>,
    mut report: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    tokio::time::timeout(pairing::CONFIRM_TIMEOUT, async {
        let pending = if let Some(invite) = invite {
            pairing::request(endpoint, &invite, name, friends).await?
        } else {
            let invite = FriendInvite::new(endpoint, name.to_owned())?;
            let link = invite.to_link()?;
            let qr = pairing::qr_modules(&link)?;
            report(&format!(
                "invite:{}",
                serde_json::json!({"link":link,"qr":qr})
            ))?;
            loop {
                match pairing::accept(endpoint, &invite, friends).await {
                    Ok(pending) => break pending,
                    Err(_) => report("Pairing request rejected; invitation still open")?,
                }
            }
        };
        report(&format!(
            "confirm:{}",
            serde_json::json!({
                "peer_id": pending.peer_id, "display_name": pending.display_name,
                "code": pending.verification_code,
            })
        ))?;
        let accepted = loop {
            if let Some(accepted) = *votes.borrow() {
                break accepted;
            }
            votes.changed().await?;
        };
        pending.confirm(accepted, friends).await?;
        report("Pairing complete. Location access remains Off for new friends")?;
        Ok(())
    })
    .await?
}
