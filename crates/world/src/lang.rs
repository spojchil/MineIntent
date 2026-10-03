//! 原版语言文件：把翻译键变成玩家在客户端里看到的文字。
//!
//! 服务端发来的系统消息（死因、成就、进出服务器、命令反馈）是「翻译键 + 参数」，
//! 原版客户端按所选语言显示；物品、方块、实体在客户端里也显示本地化名。这里装
//! 进程级的一份语言表（组合根在启动时装），呈现层与世界层按它出字。
//!
//! 名字一律写成「本地化名（注册名）」：读起来和玩家看到的一样，注册名留着给命令
//! 用、也免得两样东西同名时分不清。没装语言表（测试、没有资源）时只出注册名。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 一份语言文件：翻译键 → 文字。
#[derive(Clone, Debug, Default)]
pub struct Language {
    entries: HashMap<String, String>,
}

static CURRENT: OnceLock<Language> = OnceLock::new();

impl Language {
    /// 解析原版语言 JSON（`assets/minecraft/lang/<code>.json`）。
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let entries: HashMap<String, String> = serde_json::from_slice(bytes)
            .map_err(|error| format!("语言文件不是键值 JSON：{error}"))?;
        Ok(Self { entries })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// 按原版 `TranslatableContents` 的规则填参数：`%s` 依次取，`%N$s` 按位置取，
    /// `%%` 是百分号。键不在表里用 `fallback`，再没有就原样用键。
    pub fn format(&self, key: &str, fallback: Option<&str>, args: &[String]) -> String {
        let template = self.get(key).or(fallback).unwrap_or(key);
        fill(template, args)
    }
}

fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut next = 0;
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some('s') => {
                chars.next();
                out.push_str(args.get(next).map_or("", String::as_str));
                next += 1;
            }
            Some(d) if d.is_ascii_digit() => {
                let mut digits = String::new();
                while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                    digits.push(d);
                    chars.next();
                }
                if chars.next() == Some('$') && chars.next() == Some('s') {
                    let index = digits.parse::<usize>().unwrap_or(0);
                    out.push_str(
                        index
                            .checked_sub(1)
                            .and_then(|i| args.get(i))
                            .map_or("", String::as_str),
                    );
                } else {
                    // 不认识的格式照原样留着，不吞字。
                    out.push('%');
                    out.push_str(&digits);
                }
            }
            _ => out.push('%'),
        }
    }
    out
}

/// 装进程级语言表。只装一次；再装无效。
pub fn install(language: Language) {
    let _ = CURRENT.set(language);
}

/// 当前语言表；没装时为 None。
pub fn current() -> Option<&'static Language> {
    CURRENT.get()
}

fn bare(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

impl Language {
    fn localized(&self, id: &str, prefixes: &[&str]) -> Option<&str> {
        let id = bare(id);
        prefixes
            .iter()
            .find_map(|prefix| self.get(&format!("{prefix}.minecraft.{id}")))
    }

    fn label(&self, id: &str, prefixes: &[&str]) -> String {
        match self.localized(id, prefixes) {
            Some(name) => format!("{name}（{}）", bare(id)),
            None => bare(id).to_owned(),
        }
    }

    /// 物品：`圆石（cobblestone）`。方块物品的名字在 `block.` 下。
    pub fn item(&self, id: &str) -> String {
        self.label(id, &["item", "block"])
    }

    /// 方块：`铁矿石（iron_ore）`。
    pub fn block(&self, id: &str) -> String {
        self.label(id, &["block", "item"])
    }

    /// 实体种类：`僵尸（zombie）`。
    pub fn entity(&self, id: &str) -> String {
        self.label(id, &["entity"])
    }

    /// 生物群系：`繁茂洞穴（lush_caves）`。
    pub fn biome(&self, id: &str) -> String {
        self.label(id, &["biome"])
    }
}

/// 按当前语言表写物品名；没装表时只出注册名。
pub fn item(id: &str) -> String {
    current().map_or_else(|| bare(id).to_owned(), |language| language.item(id))
}

/// 按当前语言表写方块名；没装表时只出注册名。
pub fn block(id: &str) -> String {
    current().map_or_else(|| bare(id).to_owned(), |language| language.block(id))
}

/// 按当前语言表写实体种类；没装表时只出注册名。
pub fn entity(id: &str) -> String {
    current().map_or_else(|| bare(id).to_owned(), |language| language.entity(id))
}

/// 按当前语言表写生物群系；没装表时只出注册名。
pub fn biome(id: &str) -> String {
    current().map_or_else(|| bare(id).to_owned(), |language| language.biome(id))
}

#[cfg(feature = "azalea")]
pub(crate) use component::render;

#[cfg(feature = "azalea")]
mod component {
    use azalea::FormattedText;
    use azalea_chat::translatable_component::PrimitiveOrComponent;

    /// 把聊天组件渲染成玩家看到的文字：翻译键按当前语言表，没装表时用 Azalea
    /// 自带的英文。样式丢掉，只留文字。
    pub(crate) fn render(text: &FormattedText) -> String {
        let Some(language) = super::current() else {
            return text.to_string();
        };
        let mut out = String::new();
        render_into(text, language, &mut out);
        out
    }

    fn render_into(text: &FormattedText, language: &super::Language, out: &mut String) {
        let base = match text {
            FormattedText::Text(text) => {
                out.push_str(&text.text);
                &text.base
            }
            FormattedText::Translatable(translatable) => {
                let args: Vec<String> = translatable
                    .args
                    .iter()
                    .map(|arg| match arg {
                        PrimitiveOrComponent::FormattedText(component) => {
                            let mut rendered = String::new();
                            render_into(component, language, &mut rendered);
                            rendered
                        }
                        other => other.to_string(),
                    })
                    .collect();
                out.push_str(&language.format(
                    &translatable.key,
                    translatable.fallback.as_deref(),
                    &args,
                ));
                &translatable.base
            }
        };
        for sibling in &base.siblings {
            render_into(sibling, language, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn fills_like_vanilla() {
        assert_eq!(
            fill("%s 被 %s 杀死了", &args(&["甲", "僵尸"])),
            "甲 被 僵尸 杀死了"
        );
        assert_eq!(
            fill("%2$s 先，%1$s 后", &args(&["一", "二"])),
            "二 先，一 后"
        );
        assert_eq!(fill("100%% 完成", &[]), "100% 完成");
        assert_eq!(fill("缺参数：%s", &[]), "缺参数：");
        assert_eq!(fill("结尾 %", &[]), "结尾 %");
    }

    #[test]
    fn falls_back_to_the_fallback_then_the_key() {
        let language =
            Language::from_json(r#"{"death.attack.generic": "%1$s死了"}"#.as_bytes()).unwrap();
        assert_eq!(
            language.format("death.attack.generic", None, &args(&["甲"])),
            "甲死了"
        );
        assert_eq!(
            language.format("missing", Some("备用 %s"), &args(&["x"])),
            "备用 x"
        );
        assert_eq!(language.format("missing", None, &[]), "missing");
    }

    #[test]
    fn names_carry_the_registry_id() {
        let language = Language::from_json(
            r#"{"block.minecraft.cobblestone": "圆石", "item.minecraft.raw_iron": "粗铁",
                "entity.minecraft.zombie": "僵尸"}"#
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            language.item("minecraft:cobblestone"),
            "圆石（cobblestone）"
        );
        assert_eq!(language.item("raw_iron"), "粗铁（raw_iron）");
        assert_eq!(language.block("cobblestone"), "圆石（cobblestone）");
        assert_eq!(language.entity("zombie"), "僵尸（zombie）");
        assert_eq!(language.biome("minecraft:taiga"), "taiga");
        assert_eq!(language.item("not_a_thing"), "not_a_thing");
    }
}
