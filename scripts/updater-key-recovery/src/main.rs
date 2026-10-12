use base64::{engine::general_purpose::STANDARD, Engine as _};
use minisign::{PublicKey, SecretKeyBox};
use std::{env, error::Error, fs, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let private_path = args.next().ok_or("missing private-key path")?;
    let public_path = args.next().ok_or("missing public-key output path")?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }

    let private_text = fs::read_to_string(&private_path)?;
    let decoded = STANDARD.decode(private_text.trim())?;
    let key_text = String::from_utf8(decoded)?;
    let key_box = SecretKeyBox::from_string(&key_text)?;
    // The original CLI reported that this key was generated without a password.
    let secret_key = key_box.into_secret_key(Some(String::new()))?;
    let public_key = PublicKey::from_secret_key(&secret_key)?;
    let public_box = public_key.to_box()?.to_string();
    let encoded_public = STANDARD.encode(public_box.as_bytes());

    let public_path = Path::new(&public_path);
    if public_path.exists() {
        return Err(format!("refusing to overwrite existing public key: {}", public_path.display()).into());
    }
    fs::write(public_path, format!("{encoded_public}\n"))?;
    // Print only the public key. Never print private-key material.
    println!("{encoded_public}");
    Ok(())
}
