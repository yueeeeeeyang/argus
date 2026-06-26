//! 文件职责：提供 Argus 自动升级 manifest 签名辅助命令。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：读取发布私钥签名 manifest，并输出客户端需要配置的固定 Ed25519 验签公钥。

use std::env;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use ed25519_dalek::{Signer, SigningKey};

/// 发布私钥环境变量，内容为 32 字节 Ed25519 seed 的 Base64；也兼容 64 字节 keypair bytes。
const SIGNING_KEY_ENV: &str = "ARGUS_UPDATE_SIGNING_KEY_BASE64";
/// 可选固定公钥环境变量；设置后脚本会校验发布私钥派生出的公钥必须与它一致。
const PUBLIC_KEY_ENV: &str = "ARGUS_UPDATE_PUBLIC_KEY_BASE64";

/// 命令入口，保持错误信息直达脚本调用方。
fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        bail!("缺少命令");
    };

    match command.as_str() {
        "sign" => {
            let manifest_path = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("缺少 manifest 路径"))?;
            let signature_path = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("缺少签名输出路径"))?;
            if args.next().is_some() {
                bail!("sign 命令只接受 manifest 和签名输出两个参数");
            }
            sign_manifest(Path::new(&manifest_path), Path::new(&signature_path))
        }
        "pub-key" => {
            let signing_key = signing_key_from_env()?;
            let public_key_base64 = fixed_public_key_base64(&signing_key)?;
            println!("{PUBLIC_KEY_ENV}={public_key_base64}");
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(())
        }
        _ => {
            print_usage();
            bail!("未知命令：{command}");
        }
    }
}

/// 输出命令使用说明。
fn print_usage() {
    eprintln!(
        "用法：\n  cargo run --quiet --bin argus_update_manifest_tool -- sign <manifest-v1.json> <manifest-v1.json.sig>\n  cargo run --quiet --bin argus_update_manifest_tool -- pub-key\n\n环境变量：\n  {SIGNING_KEY_ENV}=<32字节Ed25519 seed的Base64>\n  {PUBLIC_KEY_ENV}=<可选，固定客户端验签公钥Base64>"
    );
}

/// 对 manifest 原始字节签名，并写出 Base64 文本签名文件。
fn sign_manifest(manifest_path: &Path, signature_path: &Path) -> Result<()> {
    let signing_key = signing_key_from_env()?;
    let public_key_base64 = fixed_public_key_base64(&signing_key)?;
    let manifest_bytes = fs::read(manifest_path)
        .with_context(|| format!("无法读取 manifest：{}", manifest_path.display()))?;
    let signature = signing_key.sign(&manifest_bytes);

    if let Some(parent) = signature_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建签名目录：{}", parent.display()))?;
    }
    fs::write(
        signature_path,
        format!("{}\n", BASE64_STANDARD.encode(signature.to_bytes())),
    )
    .with_context(|| format!("无法写出签名文件：{}", signature_path.display()))?;

    println!("{PUBLIC_KEY_ENV}={public_key_base64}");
    println!("manifest_signature={}", signature_path.display());
    Ok(())
}

/// 从环境变量中读取 Ed25519 发布私钥。
fn signing_key_from_env() -> Result<SigningKey> {
    let raw = env::var(SIGNING_KEY_ENV).with_context(|| {
        format!("请先设置 {SIGNING_KEY_ENV}，内容为 32 字节 Ed25519 seed 的 Base64")
    })?;
    let key_bytes = BASE64_STANDARD
        .decode(raw.trim())
        .with_context(|| format!("{SIGNING_KEY_ENV} 不是有效 Base64"))?;

    match key_bytes.len() {
        32 => {
            let seed: [u8; 32] = key_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("无法转换 32 字节 Ed25519 seed"))?;
            Ok(SigningKey::from_bytes(&seed))
        }
        64 => {
            let seed: [u8; 32] = key_bytes[..32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("无法提取 64 字节 keypair 中的 seed"))?;
            let signing_key = SigningKey::from_bytes(&seed);
            let expected_public_key = signing_key.verifying_key().to_bytes();
            if key_bytes[32..] != expected_public_key {
                bail!("{SIGNING_KEY_ENV} 的后 32 字节与 seed 派生出的公钥不一致");
            }
            Ok(signing_key)
        }
        length => bail!("{SIGNING_KEY_ENV} 解码后应为 32 或 64 字节，当前为 {length} 字节"),
    }
}

/// 返回发布私钥派生出的固定客户端验签公钥，并在设置了公钥环境变量时做一致性校验。
fn fixed_public_key_base64(signing_key: &SigningKey) -> Result<String> {
    let actual = BASE64_STANDARD.encode(signing_key.verifying_key().to_bytes());
    if let Ok(expected) = env::var(PUBLIC_KEY_ENV) {
        let expected = expected.trim();
        if !expected.is_empty() && expected != actual {
            bail!("{PUBLIC_KEY_ENV} 与发布私钥派生出的公钥不一致，请确认使用的是同一套密钥");
        }
    }
    Ok(actual)
}
