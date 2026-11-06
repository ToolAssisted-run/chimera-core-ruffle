// Fonts the project holds (issue chimera#525). A Flash movie draws most of its
// text with fonts from the computer it runs on, "device fonts", by name or as
// _sans, _serif and _typewriter. The sandbox gives the core no computer fonts,
// so that a movie draws the same text on every machine, and the one font
// ruffle carries (Noto Sans, a subset) has no Chinese, Japanese or Korean
// letters: such text was left out.
//
// So a person adds the fonts a movie needs to the project's "fonts" slot
// (file_slots.json), and they are the device fonts here:
// - each face of each file is a device font under every family name the file
//   gives it, in any language (a Chinese font is asked for by its English name,
//   "SimSun", or by its Chinese one), as bold and italic as the file says;
// - and each regular face is added after Noto Sans to every default font, so
//   text in _sans, or in a font nobody has, takes the letters Noto Sans lacks
//   from them (patch 0009 makes a default font the whole list).
// The project pins each file's SHA-1, so a movie replays with the same fonts.
use std::sync::Arc;

use ruffle_core::backend::ui::FontDefinition;
use ruffle_core::font::{DefaultFont, FontFileData};
use ruffle_core::Player;

use crate::navigator::read_whole;

// one copy of the file's bytes, however many names its faces are registered by
struct Shared(Arc<[u8]>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// The names of the files in the project's "fonts" slot, from the slot map the
/// engine mounts as "slots" ({"fonts": ["simsun.ttc"], ...}). None when the
/// project has no fonts, or when there is no project (the gate's runner).
fn slot_files() -> Vec<String> {
    let Ok(bytes) = read_whole("slots") else { return Vec::new() };
    let Ok(map) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Vec::new() };
    map.get("fonts")
        .and_then(|v| v.as_array())
        .map(|names| names.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// Every family name a face gives itself, in every language, once each.
fn family_names(face: &ttf_parser::Face) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for record in face.names() {
        if record.name_id != ttf_parser::name_id::FAMILY
            && record.name_id != ttf_parser::name_id::TYPOGRAPHIC_FAMILY
        {
            continue;
        }
        if let Some(name) = record.to_string() {
            let name = name.trim().to_owned();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

pub fn load(player: &mut Player) {
    let mut fallbacks: Vec<String> = Vec::new();
    for file in slot_files() {
        let bytes: Arc<[u8]> = match read_whole(&file) {
            Ok(b) if !b.is_empty() => b.into(),
            _ => {
                eprintln!("font {file}: the project names it but it could not be read");
                continue;
            }
        };
        let faces = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        for index in 0..faces {
            let face = match ttf_parser::Face::parse(&bytes, index) {
                Ok(face) => face,
                Err(e) => {
                    eprintln!("font {file} ({index}): not a font ruffle can read: {e}");
                    continue;
                }
            };
            let (is_bold, is_italic) = (face.is_bold(), face.is_italic());
            let names = family_names(&face);
            for name in &names {
                player.register_device_font(FontDefinition::FontFile {
                    name: name.clone(),
                    is_bold,
                    is_italic,
                    data: FontFileData::new(Shared(bytes.clone())),
                    index,
                });
            }
            if let Some(first) = names.first() {
                if !is_bold && !is_italic && !fallbacks.contains(first) {
                    fallbacks.push(first.clone());
                }
            }
        }
    }
    if fallbacks.is_empty() {
        return;
    }
    for default in [
        DefaultFont::Sans,
        DefaultFont::Serif,
        DefaultFont::Typewriter,
        DefaultFont::JapaneseGothic,
        DefaultFont::JapaneseGothicMono,
        DefaultFont::JapaneseMincho,
    ] {
        let mut names = vec!["Noto Sans".to_string()];
        names.extend(fallbacks.iter().cloned());
        player.set_default_font(default, names);
    }
}
