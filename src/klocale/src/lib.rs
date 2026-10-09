//! 本地化：字符串表 + 当前语言 + `tr`。
//!
//! ```ignore
//! // 资源：assets/locales/zh-CN.ftl、en.ftl（Fluent 的一个子集，见 [`ftl`]）
//! manager.add_loader(klocale::FtlLoader);
//! let zh = manager.request::<klocale::StringTable>("assets/locales/zh-CN.ftl");
//! // 加载完以后
//! klocale::insert(&zh.data_ref().unwrap());
//! klocale::set_language("zh-CN");
//!
//! ui.label(tr!("menu-start"));
//! ui.label(tr!("greeting", name = player.name));
//! ```
//!
//! 换语言只是改一个全局状态：界面每帧都重新布局，下一帧所有 `tr!` 就是新语言、
//! 文字长短变了布局自己跟着变，不需要通知谁。要缓存翻译结果的地方比对 [`version`]。
//!
//! # 找不到怎么办
//!
//! 先找当前语言，再找它的「上级」（`zh-CN` → `zh`），再按 [`set_fallback`] 给的顺序找；
//! 都没有就返回**键本身**——界面上露出 `menu-start` 比露出空白好找得多。
//!
//! 脚本里是 `tr("menu-start")` / `tr("greeting", { name: "小明" })`（kscript 接的同一份全局状态）。

pub mod ftl;

use kasset::{BoxedLoaderFuture, LoadError, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::{Uuid, uuid};
use std::collections::HashMap;
use std::fmt::{Display, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub use ftl::{Messages, ParseError, Piece};

/// [`StringTable`] 的资源类型标识。
pub const STRING_TABLE_TYPE_UUID: Uuid = uuid!("b7d4e2a9-3c61-4f05-8e1a-9d2c7f40a6b3");

/// 一种语言的一张字符串表（一个 `.ftl` 文件）。
#[derive(Debug, Clone, PartialEq)]
pub struct StringTable {
    /// 语言标签（`zh-CN`、`en`……）。从文件加载时取文件名（不含扩展名）。
    pub language: String,
    pub messages: Messages,
}

impl StringTable {
    /// 从 `.ftl` 文本解析。
    pub fn parse(language: impl Into<String>, source: &str) -> Result<Self, ParseError> {
        Ok(Self {
            language: language.into(),
            messages: ftl::parse(source)?,
        })
    }
}

impl ResourceData for StringTable {
    fn type_uuid(&self) -> Uuid {
        STRING_TABLE_TYPE_UUID
    }
}

/// `.ftl` 文件的加载器。语言标签取文件名：`locales/zh-CN.ftl` → `zh-CN`。
#[derive(Debug, Default)]
pub struct FtlLoader;

impl ResourceLoader for FtlLoader {
    fn extensions(&self) -> &[&str] {
        &["ftl"]
    }

    fn data_type_uuid(&self) -> Uuid {
        STRING_TABLE_TYPE_UUID
    }

    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let bytes = io.load_file(&path).await?;
            let source = String::from_utf8(bytes).map_err(LoadError::custom)?;
            let table =
                StringTable::parse(language_of(&path), &source).map_err(LoadError::custom)?;
            Ok(Box::new(table) as Box<dyn ResourceData>)
        })
    }
}

fn language_of(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 一组语言的字符串表和「当前用哪种」。全局那一份见 [`tr`] 等自由函数；测试、工具里可以各建各的。
#[derive(Debug, Clone, Default)]
pub struct Locale {
    /// 每种语言可以有好几张表（按模块分文件），后插入的同名键覆盖先前的。
    tables: HashMap<String, Messages>,
    language: String,
    fallback: Vec<String>,
    version: u64,
}

impl Locale {
    pub fn new() -> Self {
        Self::default()
    }

    /// 加一张表。同一语言已有的键被覆盖（后加载的模块可以改写前面的文案）。
    pub fn insert(&mut self, table: &StringTable) {
        let entry = self.tables.entry(table.language.clone()).or_default();
        for (key, pieces) in &table.messages.entries {
            entry.entries.insert(key.clone(), pieces.clone());
        }
        self.version += 1;
    }

    pub fn set_language(&mut self, language: impl Into<String>) {
        let language = language.into();
        if language != self.language {
            self.language = language;
            self.version += 1;
        }
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// 当前语言找不到时依次再找哪些语言（通常是 `["en"]` 或游戏的原文语言）。
    pub fn set_fallback(&mut self, languages: &[&str]) {
        self.fallback = languages.iter().map(|l| (*l).to_string()).collect();
        self.version += 1;
    }

    /// 已经有表的语言。
    pub fn languages(&self) -> Vec<&str> {
        let mut languages: Vec<&str> = self.tables.keys().map(String::as_str).collect();
        languages.sort_unstable();
        languages
    }

    /// 语言或表变了就加一：缓存了翻译结果的地方比对它决定要不要重新取。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 查找顺序：当前语言、它的上级（`zh-CN` → `zh`）、回退语言（同样带上级）。
    fn lookup(&self, key: &str) -> Option<&[Piece]> {
        let mut candidates: Vec<&str> = Vec::new();
        for language in
            std::iter::once(self.language.as_str()).chain(self.fallback.iter().map(String::as_str))
        {
            let mut tag = language;
            loop {
                if !tag.is_empty() && !candidates.contains(&tag) {
                    candidates.push(tag);
                }
                match tag.rfind('-') {
                    Some(cut) => tag = &tag[..cut],
                    None => break,
                }
            }
        }
        candidates
            .into_iter()
            .find_map(|language| self.tables.get(language)?.get(key))
    }

    /// 有没有这个键（当前语言或回退语言里）。
    pub fn has(&self, key: &str) -> bool {
        self.lookup(key).is_some()
    }

    /// 翻译。找不到时返回键本身。
    pub fn tr(&self, key: &str) -> String {
        self.tr_with(key, &[])
    }

    /// 带参数的翻译：`{ $name }` 换成 `args` 里同名的值；没给的参数原样留成 `{$name}`。
    pub fn tr_with(&self, key: &str, args: &[(&str, &dyn Display)]) -> String {
        let Some(pieces) = self.lookup(key) else {
            return key.to_string();
        };
        let mut out = String::new();
        for piece in pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Variable(name) => match args.iter().find(|(arg, _)| arg == name) {
                    Some((_, value)) => {
                        let _ = write!(out, "{value}");
                    }
                    None => {
                        let _ = write!(out, "{{${name}}}");
                    }
                },
            }
        }
        out
    }
}

static GLOBAL: RwLock<Option<Locale>> = RwLock::new(None);

fn read<R>(f: impl FnOnce(&Locale) -> R) -> R {
    let guard = GLOBAL.read().unwrap_or_else(|poison| poison.into_inner());
    match guard.as_ref() {
        Some(locale) => f(locale),
        None => f(&Locale::default()),
    }
}

fn write<R>(f: impl FnOnce(&mut Locale) -> R) -> R {
    let mut guard = GLOBAL.write().unwrap_or_else(|poison| poison.into_inner());
    f(guard.get_or_insert_with(Locale::default))
}

/// 往全局加一张表。
pub fn insert(table: &StringTable) {
    write(|locale| locale.insert(table));
}

/// 切换全局的当前语言。
pub fn set_language(language: impl Into<String>) {
    let language = language.into();
    write(|locale| locale.set_language(language));
}

/// 全局的当前语言。
pub fn language() -> String {
    read(|locale| locale.language().to_string())
}

/// 全局的回退语言。
pub fn set_fallback(languages: &[&str]) {
    write(|locale| locale.set_fallback(languages));
}

/// 全局已经有表的语言。
pub fn languages() -> Vec<String> {
    read(|locale| locale.languages().into_iter().map(str::to_string).collect())
}

/// 全局的版本号（语言或表变了就加一）。
pub fn version() -> u64 {
    read(Locale::version)
}

/// 全局里有没有这个键。
pub fn has(key: &str) -> bool {
    read(|locale| locale.has(key))
}

/// 按全局的当前语言翻译。找不到返回键本身。
pub fn tr(key: &str) -> String {
    read(|locale| locale.tr(key))
}

/// 按全局的当前语言翻译，带参数。
pub fn tr_with(key: &str, args: &[(&str, &dyn Display)]) -> String {
    read(|locale| locale.tr_with(key, args))
}

/// `tr!("key")`、`tr!("key", name = value, count = n)`。
#[macro_export]
macro_rules! tr {
    ($key:expr) => {
        $crate::tr($key)
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::tr_with($key, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locale() -> Locale {
        let mut locale = Locale::new();
        locale.insert(
            &StringTable::parse(
                "en",
                "start = Start\ngreeting = Hello, { $name }!\nonly-en = English only\n",
            )
            .unwrap(),
        );
        locale.insert(
            &StringTable::parse("zh", "start = 开始\ngreeting = 你好，{ $name }！\n").unwrap(),
        );
        locale.insert(&StringTable::parse("zh-TW", "start = 開始\n").unwrap());
        locale.set_fallback(&["en"]);
        locale
    }

    #[test]
    fn looks_up_the_current_language_then_parents_then_fallback() {
        let mut locale = locale();
        locale.set_language("zh-TW");
        assert_eq!(locale.tr("start"), "開始");
        // zh-TW 没有 greeting：退到 zh。
        assert_eq!(
            locale.tr_with("greeting", &[("name", &"小明")]),
            "你好，小明！"
        );
        // zh 也没有：退到回退语言 en。
        assert_eq!(locale.tr("only-en"), "English only");
        // 哪都没有：返回键本身。
        assert_eq!(locale.tr("missing-key"), "missing-key");
        assert!(!locale.has("missing-key"));
    }

    #[test]
    fn arguments_fill_placeholders_and_missing_ones_stay_visible() {
        let mut locale = locale();
        locale.set_language("en");
        assert_eq!(locale.tr_with("greeting", &[("name", &42)]), "Hello, 42!");
        assert_eq!(locale.tr("greeting"), "Hello, {$name}!");
    }

    #[test]
    fn later_tables_override_and_bump_the_version() {
        let mut locale = locale();
        locale.set_language("en");
        let before = locale.version();
        locale.insert(&StringTable::parse("en", "start = Play\n").unwrap());
        assert_eq!(locale.tr("start"), "Play");
        assert_eq!(
            locale.tr("only-en"),
            "English only",
            "同语言的新表只覆盖它有的键"
        );
        assert!(locale.version() > before);
        let after = locale.version();
        locale.set_language("en");
        assert_eq!(locale.version(), after, "语言没变，版本不变");
        assert_eq!(locale.languages(), vec!["en", "zh", "zh-TW"]);
    }

    #[test]
    fn the_global_locale_and_the_macro() {
        insert(&StringTable::parse("xx-test", "hp = 生命 { $value } / { $max }\n").unwrap());
        set_language("xx-test");
        let (value, max) = (30, 100);
        assert_eq!(tr!("hp", value = value, max = max), "生命 30 / 100");
        assert_eq!(tr!("hp-missing"), "hp-missing");
        assert_eq!(language(), "xx-test");
    }

    #[test]
    fn the_loader_takes_the_language_from_the_file_name() {
        use kasset::{MemoryResourceIo, ResourceManager};
        let mut io = MemoryResourceIo::new();
        io.add("locales/zh-CN.ftl", "title = 海岛\n");
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(FtlLoader);
        let table = manager
            .request_blocking::<StringTable>("locales/zh-CN.ftl")
            .unwrap();
        let table = table.data_ref().unwrap();
        assert_eq!(table.language, "zh-CN");
        let mut locale = Locale::new();
        locale.insert(&table);
        locale.set_language("zh-CN");
        assert_eq!(locale.tr("title"), "海岛");
    }
}
