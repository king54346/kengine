//! 不可变字符串及不可变字符串存储。详见 [`ImmutableString`] 和
//! [`ImmutableStringStorage`] 的文档。

#![warn(missing_docs)]

use crate::{visitor::prelude::*, safelock::SafeLock};
use fxhash::{FxHashMap, FxHasher};
use serde::{Deserialize, Serialize};
use std::{
    fmt::{Debug, Display, Formatter},
    hash::{Hash, Hasher},
    ops::Deref,
    sync::{Arc, LazyLock},
};
use parking_lot::Mutex;

#[derive(Clone, Debug)]
struct State {
    string: String,
    hash: u64,
}

/// 不可变字符串是内容固定不变的字符串。不可变性带来以下优良特性：
///
/// - 字符串地址可作为哈希值，哈希性能大幅提升，复杂度接近 O(1)
/// - 相等性比较的复杂度也变为常数
/// - 唯一性保证——多次调用 `ImmutableString::new("foo")` 只会分配一次内存，
///   后续调用复用已存在的字符串
///
/// # 使用场景
///
/// 最常见的使用场景是性能敏感的哈希映射键。
#[derive(Clone)]
pub struct ImmutableString(Arc<State>);

impl Display for ImmutableString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.string.as_ref())
    }
}

impl Debug for ImmutableString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.0.string, f)
    }
}

impl From<ImmutableString> for String {
    fn from(value: ImmutableString) -> Self {
        value.0.string.clone()
    }
}

impl Visit for ImmutableString {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        // Serialize/deserialize as ordinary string.
        let mut string = self.0.string.clone();
        string.visit(name, visitor)?;

        // Deduplicate on deserialization.
        if visitor.is_reading() {
            *self = SSTORAGE.safe_lock().insert(string);
        }

        Ok(())
    }
}

impl Serialize for ImmutableString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ImmutableString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(ImmutableString::new(
            deserializer.deserialize_string(ImmutableStringVisitor {})?,
        ))
    }
}

struct ImmutableStringVisitor {}

impl serde::de::Visitor<'_> for ImmutableStringVisitor {
    type Value = ImmutableString;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "a string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(ImmutableString::new(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }
}

impl Default for ImmutableString {
    fn default() -> Self {
        Self::new("")
    }
}

impl AsRef<str> for ImmutableString {
    fn as_ref(&self) -> &str {
        self.deref()
    }
}

impl ImmutableString {
    /// 根据给定的字符串切片创建新的不可变字符串。
    ///
    /// # 性能
    ///
    /// 此方法的均摊复杂度为 `O(1)`。在最坏情况下（后台存储中不存在该字符串时），
    /// 它需要分配内存，复杂度可能由当前内存分配器决定。
    #[inline]
    pub fn new<S: AsRef<str>>(string: S) -> ImmutableString {
        SSTORAGE.safe_lock().insert(string)
    }

    /// 返回字符串的唯一标识符。请注意：该唯一性仅在单次运行期间成立，
    /// 在应用多次运行之间不保证一致。
    #[inline]
    pub fn cached_hash(&self) -> u64 {
        self.0.hash
    }

    /// 将内部不可变字符串的内容克隆为可变字符串。
    #[inline]
    pub fn to_mutable(&self) -> String {
        self.0.string.clone()
    }

    /// 获取内部 `str` 的引用。
    pub fn as_str(&self) -> &str {
        self.deref()
    }
}

impl From<&str> for ImmutableString {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ImmutableString {
    fn from(value: String) -> Self {
        SSTORAGE.safe_lock().insert_owned(value)
    }
}

impl From<&String> for ImmutableString {
    fn from(value: &String) -> Self {
        SSTORAGE.safe_lock().insert(value)
    }
}

impl Deref for ImmutableString {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.0.string.as_ref()
    }
}

impl Hash for ImmutableString {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.cached_hash())
    }
}

impl PartialEq for ImmutableString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.cached_hash() == other.cached_hash()
    }
}

impl Eq for ImmutableString {}

/// 不可变字符串存储是应用中所有不可变字符串的后端存储，且该存储是单例。
/// 正常情况下你不应直接使用它。
#[derive(Default)]
pub struct ImmutableStringStorage {
    vec: FxHashMap<u64, Arc<State>>,
}

impl ImmutableStringStorage {
    #[inline]
    fn insert<S: AsRef<str>>(&mut self, string: S) -> ImmutableString {
        let mut hasher = FxHasher::default();
        string.as_ref().hash(&mut hasher);
        let hash = hasher.finish();

        if let Some(existing) = self.vec.get(&hash) {
            ImmutableString(existing.clone())
        } else {
            let immutable = Arc::new(State {
                string: string.as_ref().to_owned(),
                hash,
            });
            self.vec.insert(hash, immutable.clone());
            ImmutableString(immutable)
        }
    }
    /// 插入给定的 `String`，且不额外复制其内容。
    #[inline]
    fn insert_owned(&mut self, string: String) -> ImmutableString {
        let mut hasher = FxHasher::default();
        string.hash(&mut hasher);
        let hash = hasher.finish();

        if let Some(existing) = self.vec.get(&hash) {
            ImmutableString(existing.clone())
        } else {
            let immutable = Arc::new(State { string, hash });
            self.vec.insert(hash, immutable.clone());
            ImmutableString(immutable)
        }
    }
}

impl ImmutableStringStorage {
    /// 返回存储中不可变字符串的总数量。
    pub fn entry_count() -> usize {
        SSTORAGE.safe_lock().vec.len()
    }
}

static SSTORAGE: LazyLock<Arc<Mutex<ImmutableStringStorage>>> =
    LazyLock::new(|| Arc::new(Mutex::new(ImmutableStringStorage::default())));

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_immutable_string_distinctness() {
        let a = ImmutableString::new("Foobar");
        let b = ImmutableString::new("rabooF");

        assert_ne!(a.cached_hash(), b.cached_hash())
    }

    #[test]
    fn test_immutable_string_uniqueness() {
        let a = ImmutableString::new("Foobar");
        let b = ImmutableString::new("Foobar");

        // All tests share the same ImmutableStringStorage, so there is no way
        // to know what this value should be. It depends on the order the test
        // are run.
        // assert_eq!(ImmutableStringStorage::entry_count(), 2);
        assert_eq!(a.cached_hash(), b.cached_hash())
    }

    #[test]
    fn test_immutable_string_uniqueness_from_owned() {
        let a = ImmutableString::new("Foobar");
        let b = ImmutableString::from("Foobar".to_owned());

        assert_eq!(a.cached_hash(), b.cached_hash())
    }

    #[test]
    fn visit_for_immutable_string() {
        let mut a = ImmutableString::new("Foobar");
        let mut visitor = Visitor::default();

        assert!(a.visit("name", &mut visitor).is_ok());
    }

    #[test]
    fn debug_for_immutable_string() {
        let a = ImmutableString::new("Foobar");

        assert_eq!(format!("{a:?}"), "\"Foobar\"");
    }

    #[test]
    fn debug_for_immutable_string_from_owned() {
        let a = ImmutableString::from("Foobar".to_owned());

        assert_eq!(format!("{a:?}"), "\"Foobar\"");
    }

    #[test]
    fn default_for_immutable_string() {
        let a = ImmutableString::default();

        assert_eq!(a.0.string, "");
    }

    #[test]
    fn immutable_string_to_mutable() {
        let a = ImmutableString::new("Foobar");

        assert_eq!(a.to_mutable(), String::from("Foobar"));
    }

    #[test]
    fn deref_for_immutable_string() {
        let s = "Foobar";
        let a = ImmutableString::new(s);

        assert_eq!(a.deref(), s);
    }

    #[test]
    fn eq_for_immutable_string() {
        let a = ImmutableString::new("Foobar");
        let b = ImmutableString::new("Foobar");

        assert!(a == b);
    }
}
