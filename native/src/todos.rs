//! Your todo list and the one-line editor used to type into it. Saved in %APPDATA%\TwoCode\todos.json.
use serde::{Deserialize, Serialize};
use std::fs;

const MAX_CHARS: usize = 300;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub text: String,
    pub done: bool,
}

/// Open work first, finished items below it, otherwise in the order you added them.
pub fn add(list: &mut Vec<Todo>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let id = list.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    let at = list.iter().position(|t| t.done).unwrap_or(list.len());
    list.insert(at, Todo { id, text: text.to_string(), done: false });
}

pub fn toggle(list: &mut Vec<Todo>, id: u64) {
    let Some(at) = list.iter().position(|t| t.id == id) else { return };
    let mut todo = list.remove(at);
    todo.done = !todo.done;
    let to = if todo.done { list.len() } else { list.iter().position(|t| t.done).unwrap_or(list.len()) };
    list.insert(to, todo);
}

/// Saving an emptied todo deletes it.
pub fn rewrite(list: &mut Vec<Todo>, id: u64, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        delete(list, id);
    } else if let Some(todo) = list.iter_mut().find(|t| t.id == id) {
        todo.text = text.to_string();
    }
}

pub fn delete(list: &mut Vec<Todo>, id: u64) {
    list.retain(|t| t.id != id);
}

pub fn load() -> Vec<Todo> {
    fs::read_to_string(crate::config::dir().join("todos.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn save(list: &[Todo]) {
    let dir = crate::config::dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string_pretty(list) {
        let temp = dir.join("todos.json.tmp");
        if fs::write(&temp, text).is_ok() {
            let _ = fs::rename(temp, dir.join("todos.json"));
        }
    }
}

/// What is being typed. `target` is the todo being changed, or `None` for a new one.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Edit {
    pub target: Option<u64>,
    pub chars: Vec<char>,
    pub caret: usize,
}

impl Edit {
    pub fn new(target: Option<u64>, text: &str) -> Edit {
        let chars: Vec<char> = text.chars().collect();
        Edit { target, caret: chars.len(), chars }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn before_caret(&self) -> String {
        self.chars[..self.caret].iter().collect()
    }

    /// Typed or pasted text. Line breaks become spaces, because a todo is one line.
    pub fn insert(&mut self, text: &str) {
        for c in text.chars() {
            let c = if c == '\n' || c == '\t' { ' ' } else { c };
            if c.is_control() || self.chars.len() >= MAX_CHARS {
                continue;
            }
            self.chars.insert(self.caret, c);
            self.caret += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            self.chars.remove(self.caret);
        }
    }

    pub fn delete(&mut self) {
        if self.caret < self.chars.len() {
            self.chars.remove(self.caret);
        }
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.chars.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_todos_stay_above_finished_ones() {
        let mut list = vec![];
        add(&mut list, " first ");
        add(&mut list, "   ");
        add(&mut list, "second");
        toggle(&mut list, 1);
        add(&mut list, "third");
        let order: Vec<&str> = list.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(order, ["second", "third", "first"]);
        toggle(&mut list, 1);
        assert_eq!(list.iter().position(|t| t.id == 1), Some(2));
        assert!(!list[2].done);
    }

    #[test]
    fn ids_are_not_reused_while_the_newest_exists() {
        let mut list = vec![];
        add(&mut list, "a");
        add(&mut list, "b");
        delete(&mut list, 1);
        add(&mut list, "c");
        assert_eq!(list.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn emptying_a_todo_deletes_it() {
        let mut list = vec![];
        add(&mut list, "a");
        rewrite(&mut list, 1, "changed");
        assert_eq!(list[0].text, "changed");
        rewrite(&mut list, 1, "  ");
        assert!(list.is_empty());
    }

    #[test]
    fn the_editor_moves_and_changes_text() {
        let mut e = Edit::new(None, "");
        e.insert("héllo");
        e.left();
        e.backspace();
        assert_eq!(e.text(), "hélo");
        assert_eq!(e.before_caret(), "hél");
        e.home();
        e.delete();
        e.end();
        e.insert("!");
        assert_eq!(e.text(), "élo!");
        e.right();
        assert_eq!(e.caret, 4);
    }

    #[test]
    fn pasting_stays_on_one_line_and_within_the_limit() {
        let mut e = Edit::new(None, "");
        e.insert("a\nb\r\nc");
        assert_eq!(e.text(), "a b c");
        e.insert(&"x".repeat(1000));
        assert_eq!(e.chars.len(), MAX_CHARS);
    }
}
