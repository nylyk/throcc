use std::path::{Path, PathBuf};
use std::ptr;

use anyhow::{Context, Result, bail};
use jni::objects::{JByteArray, JObject, JObjectArray, JString, JValue};
use jni::sys::{self, jint};
use jni::{Env, JavaVM, jni_sig, jni_str};
use zeroize::Zeroizing;

use super::SeedEntry;
use crate::files::{read_if_exists, replace_file};

const WRAPPED_IDENTITY_FILE: &str = "identity_key.wrapped";
const KEYSTORE: &str = "AndroidKeyStore";
const WRAPPING_KEY_ALIAS: &str = "throcc.identity";
const NONCE_LEN: usize = 12;
const TAG_BITS: jint = 128;

/// The value of `javax.crypto.Cipher.ENCRYPT_MODE`.
const ENCRYPT_MODE: jint = 1;
/// The value of `javax.crypto.Cipher.DECRYPT_MODE`.
const DECRYPT_MODE: jint = 2;
/// The value of `KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT`.
const PURPOSE_ENCRYPT_DECRYPT: jint = 1 | 2;

/// The identity key's seed in a file in `dir`, wrapped with an Android Keystore key.
pub(super) struct WrappedSeed {
    path: PathBuf,
}

pub(super) fn identity_entry(dir: &Path) -> Result<Option<WrappedSeed>> {
    Ok(Some(WrappedSeed {
        path: dir.join(WRAPPED_IDENTITY_FILE),
    }))
}

impl SeedEntry for WrappedSeed {
    fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let Some(wrapped) = read_if_exists(&self.path)? else {
            return Ok(None);
        };
        let Some((nonce, sealed)) = wrapped.split_at_checked(NONCE_LEN) else {
            bail!("the stored identity key is corrupt");
        };
        with_env(|env| unwrap(env, nonce, sealed))
    }

    fn write(&self, seed: &[u8]) -> Result<()> {
        let wrapped = with_env(|env| wrap(env, seed))?;
        if wrapped.len() != NONCE_LEN + seed.len() + TAG_BITS as usize / 8 {
            bail!(
                "the Android Keystore sealed the identity key in a shape that can't be read back"
            );
        }
        replace_file(&self.path, &wrapped)
    }
}

// libnativehelper exports this from API 31 on.
#[link(name = "nativehelper")]
unsafe extern "system" {
    fn JNI_GetCreatedJavaVMs(
        vms: *mut *mut sys::JavaVM,
        len: sys::jsize,
        count: *mut sys::jsize,
    ) -> jint;
}

fn with_env<T>(call: impl FnOnce(&mut Env) -> jni::errors::Result<T>) -> Result<T> {
    let mut vm = ptr::null_mut();
    let mut count = 0;
    // SAFETY: `vm` has room for the one VM asked for, and `count` is a valid out pointer.
    let status = unsafe { JNI_GetCreatedJavaVMs(&mut vm, 1, &mut count) };
    if status != sys::JNI_OK || count != 1 {
        bail!("no Java VM to reach the Android Keystore through");
    }
    // SAFETY: the runtime just handed out `vm`, and an Android process keeps its VM for life.
    let vm = unsafe { JavaVM::from_raw(vm) };
    vm.attach_current_thread(call)
        .context("the Android Keystore")
}

fn wrap(env: &mut Env, seed: &[u8]) -> jni::errors::Result<Vec<u8>> {
    let key = match wrapping_key(env)? {
        Some(key) => key,
        None => new_wrapping_key(env)?,
    };
    let cipher = cipher(env)?;
    env.call_method(
        &cipher,
        jni_str!("init"),
        jni_sig!("(ILjava/security/Key;)V"),
        &[JValue::Int(ENCRYPT_MODE), JValue::Object(&key)],
    )?;
    let nonce = env
        .call_method(&cipher, jni_str!("getIV"), jni_sig!("()[B"), &[])?
        .l()?;
    let nonce = env.cast_local::<JByteArray>(nonce)?;
    let mut wrapped = env.convert_byte_array(&nonce)?;
    let plain = env.byte_array_from_slice(seed)?;
    let sealed = do_final(env, &cipher, &plain);
    let sealed = once_cleared(env, &plain, sealed)?;
    wrapped.extend_from_slice(&env.convert_byte_array(&sealed)?);
    Ok(wrapped)
}

/// The seed, or `None` when the Keystore holds no wrapping key.
fn unwrap(
    env: &mut Env,
    nonce: &[u8],
    sealed: &[u8],
) -> jni::errors::Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(key) = wrapping_key(env)? else {
        return Ok(None);
    };
    let nonce = env.byte_array_from_slice(nonce)?;
    let spec = env.new_object(
        jni_str!("javax/crypto/spec/GCMParameterSpec"),
        jni_sig!("(I[B)V"),
        &[JValue::Int(TAG_BITS), JValue::Object(&nonce)],
    )?;
    let cipher = cipher(env)?;
    env.call_method(
        &cipher,
        jni_str!("init"),
        jni_sig!("(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V"),
        &[
            JValue::Int(DECRYPT_MODE),
            JValue::Object(&key),
            JValue::Object(&spec),
        ],
    )?;
    let sealed = env.byte_array_from_slice(sealed)?;
    let plain = do_final(env, &cipher, &sealed)?;
    let seed = env.convert_byte_array(&plain).map(Zeroizing::new);
    once_cleared(env, &plain, seed).map(Some)
}

fn cipher<'local>(env: &mut Env<'local>) -> jni::errors::Result<JObject<'local>> {
    let transformation = env.new_string("AES/GCM/NoPadding")?;
    env.call_static_method(
        jni_str!("javax/crypto/Cipher"),
        jni_str!("getInstance"),
        jni_sig!("(Ljava/lang/String;)Ljavax/crypto/Cipher;"),
        &[JValue::Object(&transformation)],
    )?
    .l()
}

fn do_final<'local>(
    env: &mut Env<'local>,
    cipher: &JObject,
    input: &JByteArray,
) -> jni::errors::Result<JByteArray<'local>> {
    let output = env
        .call_method(
            cipher,
            jni_str!("doFinal"),
            jni_sig!("([B)[B"),
            &[JValue::Object(input)],
        )?
        .l()?;
    env.cast_local::<JByteArray>(output)
}

/// The `result` of a call that used `held_seed`, once that Java array is overwritten.
fn once_cleared<T>(
    env: &Env,
    held_seed: &JByteArray,
    result: jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    // JNI allows no array write while an exception is pending.
    let thrown = env.exception_catch();
    let len = held_seed.len(env)?;
    held_seed.set_region(env, 0, &vec![0; len])?;
    thrown?;
    result
}

/// The Keystore's key for wrapping the seed, or `None` while it holds none.
fn wrapping_key<'local>(env: &mut Env<'local>) -> jni::errors::Result<Option<JObject<'local>>> {
    let keystore = env.new_string(KEYSTORE)?;
    let alias = env.new_string(WRAPPING_KEY_ALIAS)?;
    let store = env
        .call_static_method(
            jni_str!("java/security/KeyStore"),
            jni_str!("getInstance"),
            jni_sig!("(Ljava/lang/String;)Ljava/security/KeyStore;"),
            &[JValue::Object(&keystore)],
        )?
        .l()?;
    env.call_method(
        &store,
        jni_str!("load"),
        jni_sig!("(Ljava/security/KeyStore$LoadStoreParameter;)V"),
        &[JValue::Object(&JObject::null())],
    )?;
    let key = env
        .call_method(
            &store,
            jni_str!("getKey"),
            jni_sig!("(Ljava/lang/String;[C)Ljava/security/Key;"),
            &[JValue::Object(&alias), JValue::Object(&JObject::null())],
        )?
        .l()?;
    Ok((!key.is_null()).then_some(key))
}

/// A new AES-256-GCM key that never leaves the Keystore, replacing any under the alias.
fn new_wrapping_key<'local>(env: &mut Env<'local>) -> jni::errors::Result<JObject<'local>> {
    let keystore = env.new_string(KEYSTORE)?;
    let alias = env.new_string(WRAPPING_KEY_ALIAS)?;
    let algorithm = env.new_string("AES")?;
    let block_modes = strings(env, "GCM")?;
    let paddings = strings(env, "NoPadding")?;

    let builder = env.new_object(
        jni_str!("android/security/keystore/KeyGenParameterSpec$Builder"),
        jni_sig!("(Ljava/lang/String;I)V"),
        &[JValue::Object(&alias), JValue::Int(PURPOSE_ENCRYPT_DECRYPT)],
    )?;
    env.call_method(
        &builder,
        jni_str!("setBlockModes"),
        jni_sig!("([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
        &[JValue::Object(&block_modes)],
    )?;
    env.call_method(
        &builder,
        jni_str!("setEncryptionPaddings"),
        jni_sig!("([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
        &[JValue::Object(&paddings)],
    )?;
    env.call_method(
        &builder,
        jni_str!("setKeySize"),
        jni_sig!("(I)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
        &[JValue::Int(256)],
    )?;
    let spec = env
        .call_method(
            &builder,
            jni_str!("build"),
            jni_sig!("()Landroid/security/keystore/KeyGenParameterSpec;"),
            &[],
        )?
        .l()?;

    let generator = env
        .call_static_method(
            jni_str!("javax/crypto/KeyGenerator"),
            jni_str!("getInstance"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;"),
            &[JValue::Object(&algorithm), JValue::Object(&keystore)],
        )?
        .l()?;
    env.call_method(
        &generator,
        jni_str!("init"),
        jni_sig!("(Ljava/security/spec/AlgorithmParameterSpec;)V"),
        &[JValue::Object(&spec)],
    )?;
    env.call_method(
        &generator,
        jni_str!("generateKey"),
        jni_sig!("()Ljavax/crypto/SecretKey;"),
        &[],
    )?
    .l()
}

/// A Java `String[]` holding `value` alone.
fn strings<'local>(
    env: &mut Env<'local>,
    value: &str,
) -> jni::errors::Result<JObjectArray<'local, JString<'local>>> {
    let value = env.new_string(value)?;
    JObjectArray::<JString>::new(env, 1, &value)
}
