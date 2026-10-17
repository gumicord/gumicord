//! Android's Keystore, reached over JNI.
//!
//! One AES-GCM key per store, generated inside the Keystore and never
//! leaving it. That is the same line DPAPI draws on Windows: the sealed
//! file is noise on any other device, and taking the OS account takes this
//! too.
//!
//! No Kotlin goes with it. The calls are the platform's own, spelled out
//! here so the shell stays a single Rust library.

// Desktop builds compile this file so the calls are type-checked on the
// machine that writes them; only Android executes any of it.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use jni::objects::{JByteArray, JObject, JValue};
use jni::{jni_sig, jni_str};

use super::SecretError;
use super::blob;

/// Where Android keeps these keys. On a device with a secure element the
/// hardware holds them; elsewhere the same API is backed by software.
const KEYSTORE: &str = "AndroidKeyStore";
/// The algorithm, and the provider's name since it only exists there.
const KEY_ALGORITHM: &str = "AES";
/// GCM without padding: it authenticates, so a flipped bit fails to
/// decrypt rather than handing back a corrupted token.
const CIPHER: &str = "AES/GCM/NoPadding";
const BLOCK_MODE: &str = "GCM";
const NO_PADDING: &str = "NoPadding";
const KEY_BITS: i32 = 256;
/// The authentication tag, in bits.
const TAG_BITS: i32 = 128;
/// `KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT`.
const PURPOSE: i32 = 3;
/// `Cipher.ENCRYPT_MODE` and `Cipher.DECRYPT_MODE`.
const ENCRYPT_MODE: i32 = 1;
const DECRYPT_MODE: i32 = 2;

/// Seals a secret under the store's key. The IV comes from the cipher and
/// travels with the result.
pub(super) fn encrypt(alias: &str, plain: &[u8]) -> Result<Vec<u8>, SecretError> {
    with_env(|env| {
        let key = fetch_key(env, alias)?;
        let algorithm = env.new_string(CIPHER)?;
        let cipher = env.new_object(
            jni_str!("javax/crypto/Cipher"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::from(&algorithm)],
        )?;
        env.call_method(
            &cipher,
            jni_str!("init"),
            jni_sig!("(ILjava/security/Key;)V"),
            &[JValue::Int(ENCRYPT_MODE), JValue::from(&key)],
        )?;

        let plain = env.byte_array_from_slice(plain)?;
        let iv: JObject = env
            .call_method(&cipher, jni_str!("getIV"), jni_sig!("()[B"), &[])?
            .l()?;
        let iv = env.cast_local::<JByteArray>(iv)?;
        let iv = env.convert_byte_array(&iv)?;
        let out: JObject = env
            .call_method(
                &cipher,
                jni_str!("doFinal"),
                jni_sig!("([B)[B"),
                &[JValue::from(&plain)],
            )?
            .l()?;
        let out = env.cast_local::<JByteArray>(out)?;
        let out = env.convert_byte_array(&out)?;
        Ok(blob::pack(&iv, &out))
    })
}

/// Opens a sealed secret again. A file whose key is gone — a backup
/// restored onto another device — fails here, which is the right answer:
/// the caller logs in again.
pub(super) fn decrypt(alias: &str, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
    let (iv, cipher_text) = blob::split(sealed).ok_or(SecretError::Keystore)?;
    with_env(|env| {
        let key = fetch_key(env, alias)?;
        let algorithm = env.new_string(CIPHER)?;
        let cipher = env.new_object(
            jni_str!("javax/crypto/Cipher"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::from(&algorithm)],
        )?;
        let iv = env.byte_array_from_slice(iv)?;
        let params = env.new_object(
            jni_str!("javax/crypto/spec/GCMParameterSpec"),
            jni_sig!("(I[B)V"),
            &[JValue::Int(TAG_BITS), JValue::from(&iv)],
        )?;
        env.call_method(
            &cipher,
            jni_str!("init"),
            jni_sig!("(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V"),
            &[
                JValue::Int(DECRYPT_MODE),
                JValue::from(&key),
                JValue::from(&params),
            ],
        )?;

        let sealed = env.byte_array_from_slice(cipher_text)?;
        let out: JObject = env
            .call_method(
                &cipher,
                jni_str!("doFinal"),
                jni_sig!("([B)[B"),
                &[JValue::from(&sealed)],
            )?
            .l()?;
        let out = env.cast_local::<JByteArray>(out)?;
        env.convert_byte_array(&out).map_err(SecretError::from)
    })
}

/// Runs `f` against the running activity. The Keystore needs no window and
/// no looper; any attached thread will do.
fn with_env<T>(
    f: impl FnOnce(&mut jni::Env<'_>) -> Result<T, SecretError>,
) -> Result<T, SecretError> {
    let ctx = ndk_context::android_context();
    // Nothing has run yet on a host test binary, and from_raw asserts on null.
    if ctx.vm().is_null() {
        return Err(SecretError::Keystore);
    }
    // Safe: the shell set both pointers up before anything stored a secret.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| {
        let out = f(env);
        // A thrown exception must not be waiting for the next JNI call.
        if env.exception_check() {
            env.exception_clear();
        }
        out
    })
}

/// The store's key, made the first time anything is sealed.
fn fetch_key<'local>(
    env: &mut jni::Env<'local>,
    alias: &str,
) -> Result<JObject<'local>, SecretError> {
    let store = open_store(env)?;
    let name = env.new_string(alias)?;
    let present = env
        .call_method(
            &store,
            jni_str!("containsAlias"),
            jni_sig!("(Ljava/lang/String;)Z"),
            &[JValue::from(&name)],
        )?
        .z()?;
    if !present {
        make_key(env, alias)?;
    }
    // Reopened either way: the instance above predates the new key.
    let store = open_store(env)?;
    let name = env.new_string(alias)?;
    let key = env
        .call_method(
            &store,
            jni_str!("getKey"),
            jni_sig!("(Ljava/lang/String;[C)Ljava/security/Key;"),
            &[JValue::from(&name), JValue::from(&JObject::null())],
        )?
        .l()?;
    if key.is_null() {
        return Err(SecretError::Keystore);
    }
    Ok(key)
}

/// The Keystore itself, opened without a password.
fn open_store<'local>(env: &mut jni::Env<'local>) -> Result<JObject<'local>, SecretError> {
    let name = env.new_string(KEYSTORE)?;
    let store = env
        .call_static_method(
            jni_str!("java/security/KeyStore"),
            jni_str!("getInstance"),
            jni_sig!("(Ljava/lang/String;)Ljava/security/KeyStore;"),
            &[JValue::from(&name)],
        )?
        .l()?;
    env.call_method(
        &store,
        jni_str!("load"),
        jni_sig!("(Ljava/security/KeyStore$LoadStoreParameter;)V"),
        &[JValue::from(&JObject::null())],
    )?;
    Ok(store)
}

/// Generates the key. GCM only: an alias cannot later be asked for another
/// mode, which keeps a widened purpose from becoming a quiet downgrade.
fn make_key(env: &mut jni::Env<'_>, alias: &str) -> Result<(), SecretError> {
    let algorithm = env.new_string(KEY_ALGORITHM)?;
    let provider = env.new_string(KEYSTORE)?;
    let generator = env
        .call_static_method(
            jni_str!("javax/crypto/KeyGenerator"),
            jni_str!("getInstance"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;"),
            &[JValue::from(&algorithm), JValue::from(&provider)],
        )?
        .l()?;

    let name = env.new_string(alias)?;
    let builder = env.new_object(
        jni_str!("android/security/keystore/KeyGenParameterSpec$Builder"),
        jni_sig!("(Ljava/lang/String;I)V"),
        &[JValue::from(&name), JValue::Int(PURPOSE)],
    )?;
    let mode = env.new_string(BLOCK_MODE)?;
    let modes = env.new_object_array(1, jni_str!("java/lang/String"), &mode)?;
    env.call_method(
        &builder,
        jni_str!("setBlockModes"),
        jni_sig!("([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
        &[JValue::from(&modes)],
    )?;
    let padding = env.new_string(NO_PADDING)?;
    let paddings = env.new_object_array(1, jni_str!("java/lang/String"), &padding)?;
    env.call_method(
        &builder,
        jni_str!("setEncryptionPaddings"),
        jni_sig!("([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
        &[JValue::from(&paddings)],
    )?;
    env.call_method(
        &builder,
        jni_str!("setKeySize"),
        jni_sig!("(I)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
        &[JValue::Int(KEY_BITS)],
    )?;
    let spec = env
        .call_method(
            &builder,
            jni_str!("build"),
            jni_sig!("()Landroid/security/keystore/KeyGenParameterSpec;"),
            &[],
        )?
        .l()?;
    env.call_method(
        &generator,
        jni_str!("init"),
        jni_sig!("(Ljava/security/spec/AlgorithmParameterSpec;)V"),
        &[JValue::from(&spec)],
    )?;
    env.call_method(
        &generator,
        jni_str!("generateKey"),
        jni_sig!("()Ljavax/crypto/SecretKey;"),
        &[],
    )?;
    Ok(())
}
