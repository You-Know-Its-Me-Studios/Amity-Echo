//! References only in configuration; secrets come from environment or OS vault.
use anyhow::{Context, Result, bail, ensure};

pub const DEVELOPMENT_REFERENCE: &str = "windows:Amity/Echo/Cadenza/development";

/// Called only while Echo holds the development startup lock. A pre-existing
/// credential is never replaced, including when a service refuses it.
pub fn provision_development() -> Result<String> {
    #[cfg(windows)]
    {
        let target = DEVELOPMENT_REFERENCE.strip_prefix("windows:").unwrap();
        if let Some(secret) = vault::read_optional(target)? {
            return Ok(secret);
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).context("generating a development service credential")?;
        let secret = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        vault::write(target, &secret)?;
        Ok(secret)
    }
    #[cfg(not(windows))]
    {
        bail!("Automatic development credentials require a supported OS credential vault")
    }
}

pub fn development_credential_exists() -> Result<bool> {
    #[cfg(windows)]
    { Ok(vault::read_optional(DEVELOPMENT_REFERENCE.strip_prefix("windows:").unwrap())?.is_some()) }
    #[cfg(not(windows))]
    { Ok(false) }
}

pub fn resolve(reference: &str) -> Result<String> {
    let secret = if let Some(name) = reference.strip_prefix("env:") {
        validate_env(name)?;
        std::env::var(name).with_context(|| format!("Cadenza credential unavailable: environment variable {name} is not set"))?
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
        read_optional(name)?.context("Cadenza credential is missing from Windows Credential Manager")
    }
    pub fn read_optional(name: &str) -> Result<Option<String>> {
        let name = target(name)?;
        let mut credential = std::ptr::null_mut();
        // Win32 owns this allocation until CredFree; length is validated before a slice is formed.
        unsafe {
            if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(1168) { return Ok(None); }
                return Err(error).context("reading Cadenza credential from Windows Credential Manager");
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
            result.map(Some)
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
