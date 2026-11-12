//! References only in configuration; secrets come from environment or OS vault.
use anyhow::{Context, Result, bail, ensure};

pub fn resolve(reference: &str) -> Result<String> {
    let secret = if let Some(name) = reference.strip_prefix("env:") {
        validate_env(name)?;
        std::env::var(name).with_context(|| format!("Cadenza credential unavailable: set {name} or configure a Windows credential reference in /settings"))?
    } else if let Some(target) = reference.strip_prefix("windows:") {
        vault::read(target)?
    } else {
        bail!("Cadenza credential reference must start with env: or windows:");
    };
    ensure!(
        (32..=1024).contains(&secret.len()) && !secret.contains(['\0', '\r', '\n']),
        "Cadenza credential must contain 32..1024 bytes without control characters"
    );
    Ok(secret)
}

pub fn validate_env(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'),
        "Enter an uppercase credential environment variable name"
    );
    Ok(())
}

pub fn store_environment(name: &str, target: &str) -> Result<()> {
    let secret = resolve(&format!("env:{name}"))?;
    vault::write(target, &secret)
}

#[cfg(windows)]
mod vault {
    use super::*;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW, CredWriteW,
    };
    fn target(s: &str) -> Result<Vec<u16>> {
        ensure!(
            s.starts_with("Amity/Echo/Cadenza/") && s.len() <= 200 && !s.contains('\0'),
            "Invalid Echo Cadenza vault target"
        );
        Ok(s.encode_utf16().chain(Some(0)).collect())
    }
    pub fn read(name: &str) -> Result<String> {
        let name = target(name)?;
        let mut credential = std::ptr::null_mut();
        // Win32 owns this allocation until CredFree; length is validated before a slice is formed.
        unsafe {
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
                bail!(
                    "Cadenza credential is missing from Windows Credential Manager; configure it in /settings"
                );
            }
            ensure!(
                !credential.is_null(),
                "Windows returned an empty Cadenza credential"
            );
            let result = (|| {
                let c = &*credential;
                ensure!(
                    (32..=1024).contains(&c.CredentialBlobSize) && !c.CredentialBlob.is_null(),
                    "Invalid Cadenza vault credential"
                );
                String::from_utf8(
                    std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                        .to_vec(),
                )
                .context("Cadenza vault credential is not UTF-8")
            })();
            CredFree(credential.cast());
            result
        }
    }
    pub fn write(name: &str, secret: &str) -> Result<()> {
        let mut name = target(name)?;
        let mut c: CREDENTIALW = unsafe { std::mem::zeroed() };
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = name.as_mut_ptr();
        c.CredentialBlobSize = secret.len() as u32;
        c.CredentialBlob = secret.as_ptr() as *mut u8;
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        ensure!(
            unsafe { CredWriteW(&c, 0) } != 0,
            "Could not save Cadenza credential in Windows Credential Manager"
        );
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn windows_vault_roundtrip_with_task_owned_credential() {
        use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).unwrap();
        let secret = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let target = format!(
            "Amity/Echo/Cadenza/test-{}-{}",
            std::process::id(),
            &secret[..12]
        );
        vault::write(&target, &secret).unwrap();
        struct Cleanup(Vec<u16>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    CredDeleteW(self.0.as_ptr(), CRED_TYPE_GENERIC, 0);
                }
            }
        }
        let _cleanup = Cleanup(target.encode_utf16().chain(Some(0)).collect());
        assert!(
            resolve(&format!("windows:{target}")).unwrap() == secret,
            "Credential vault roundtrip failed"
        );
    }
    #[test]
    fn rejects_unrelated_vault_targets() {
        assert!(vault::read("unrelated/application").is_err());
    }
}
#[cfg(not(windows))]
mod vault {
    use super::*;
    pub fn read(_: &str) -> Result<String> {
        bail!("Windows credential storage is unavailable; use an env: credential reference")
    }
    pub fn write(_: &str, _: &str) -> Result<()> {
        bail!("Windows credential storage is unavailable; use an env: credential reference")
    }
}
