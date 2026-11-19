//! iOS sharing identity: the iroh secret key as a Keychain generic password. Survives app
//! updates; deleted with the app or by identity reset.
use super::Result;
use iroh::SecretKey;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

const SERVICE: &str = "dev.traveler.sharing";
const ACCOUNT: &str = "identity";

/// errSecItemNotFound: no identity stored yet.
const NOT_FOUND: i32 = -25300;

pub(super) fn load_or_create() -> Result<SecretKey> {
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => {
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| "Stored sharing identity is damaged")?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) if error.code() == NOT_FOUND => {
            let key = SecretKey::generate();
            set_generic_password(SERVICE, ACCOUNT, &key.to_bytes())?;
            Ok(key)
        }
        Err(error) => {
            super::places::log(&format!("sharing identity: Keychain read failed: {error}"));
            Err(error.into())
        }
    }
}

pub(super) fn delete() -> Result<()> {
    match delete_generic_password(SERVICE, ACCOUNT) {
        Err(error) if error.code() != NOT_FOUND => Err(error.into()),
        _ => Ok(()),
    }
}
