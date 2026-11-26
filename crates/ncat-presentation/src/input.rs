//! Translate normalized keys into reader commands. Shells map native key/modifier
//! values (including Command vs Control) before calling this policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    FramePrevious,
    FrameNext,
    BookPrevious,
    BookNext,
    Playback,
    Open,
    Network,
    Quit,
    Fullscreen,
    Escape,
    ToggleInterface,
    Tab,
    Next,
    Previous,
    Left,
    Right,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    Fit,
    Rotate,
    Direction,
    Reload,
    Delete,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub primary: bool,
    pub shift: bool,
    pub alternate: bool,
    pub system: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    FramePrevious,
    FrameNext,
    Book { forward: bool },
    Playback,
    Native,
    Block,
    Open { folder: bool },
    Network,
    Quit,
    Fullscreen,
    ToggleInterface,
    Page(super::PageKey),
    ToggleMetadata,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    Fit,
    Rotate { backwards: bool },
    Direction,
    Reload,
    Delete,
}

pub fn command(key: Key, modifiers: Modifiers, controls_have_input: bool) -> Command {
    use Command as C;
    use Key as K;
    if key == K::Tab {
        return if !modifiers.primary && !modifiers.alternate && !modifiers.system {
            C::ToggleMetadata
        } else {
            C::Block
        };
    }
    if modifiers.primary {
        match key {
            K::Open => {
                return C::Open {
                    folder: modifiers.shift,
                };
            }
            K::Network => return C::Network,
            K::Quit => return C::Quit,
            _ => {}
        }
    }
    match key {
        K::Fullscreen => return C::Fullscreen,
        K::Escape => return C::Quit,
        K::ToggleInterface => return C::ToggleInterface,
        _ => {}
    }
    if !modifiers.primary && !modifiers.alternate && !modifiers.system {
        match key {
            K::FramePrevious => return C::FramePrevious,
            K::FrameNext => return C::FrameNext,
            K::Playback => return C::Playback,
            _ => {}
        }
    }
    if controls_have_input {
        return C::Native;
    }
    if key == K::Delete {
        return if modifiers == Modifiers::default() {
            C::Delete
        } else {
            C::Native
        };
    }
    if modifiers.primary || modifiers.alternate || modifiers.system {
        return C::Native;
    }
    match key {
        K::Next => C::Page(super::PageKey::Next),
        K::Previous => C::Page(super::PageKey::Previous),
        K::Left => C::Page(super::PageKey::Left),
        K::Right => C::Page(super::PageKey::Right),
        K::First => C::First,
        K::Last => C::Last,
        K::BookPrevious | K::BookNext if !modifiers.shift => C::Book {
            forward: key == K::BookNext,
        },
        K::ZoomIn => C::ZoomIn,
        K::ZoomOut => C::ZoomOut,
        K::Fit => C::Fit,
        K::Rotate => C::Rotate {
            backwards: modifiers.shift,
        },
        K::Direction => C::Direction,
        K::Reload => C::Reload,
        _ => C::Native,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delete_only_applies_to_the_viewer_without_modifiers() {
        let plain = Modifiers::default();
        assert_eq!(command(Key::Delete, plain, false), Command::Delete);
        assert_eq!(command(Key::Delete, plain, true), Command::Native);
        assert_eq!(
            command(
                Key::Delete,
                Modifiers {
                    shift: true,
                    ..plain
                },
                false
            ),
            Command::Native
        );
    }
    #[test]
    fn brackets_navigate_books_only_when_reading_without_modifiers() {
        for (key, forward) in [(Key::BookPrevious, false), (Key::BookNext, true)] {
            assert_eq!(
                command(key, Modifiers::default(), false),
                Command::Book { forward }
            );
            assert_eq!(command(key, Modifiers::default(), true), Command::Native);
            assert_eq!(
                command(
                    key,
                    Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    },
                    false
                ),
                Command::Native
            );
        }
    }
    #[test]
    fn tab_never_navigates_controls_and_modified_tab_does_not_toggle() {
        for focused in [false, true] {
            for shift in [false, true] {
                assert_eq!(
                    command(
                        Key::Tab,
                        Modifiers {
                            shift,
                            ..Modifiers::default()
                        },
                        focused
                    ),
                    Command::ToggleMetadata
                );
            }
            for modifiers in [
                Modifiers {
                    primary: true,
                    ..Modifiers::default()
                },
                Modifiers {
                    alternate: true,
                    ..Modifiers::default()
                },
                Modifiers {
                    system: true,
                    ..Modifiers::default()
                },
            ] {
                assert_eq!(command(Key::Tab, modifiers, focused), Command::Block);
            }
        }
    }
    #[test]
    fn frame_commands_work_with_toolbar_focus_but_not_os_modifiers() {
        for (key, expected) in [
            (Key::FramePrevious, Command::FramePrevious),
            (Key::FrameNext, Command::FrameNext),
            (Key::Playback, Command::Playback),
        ] {
            assert_eq!(command(key, Modifiers::default(), true), expected);
            assert_eq!(
                command(
                    key,
                    Modifiers {
                        primary: true,
                        ..Modifiers::default()
                    },
                    true
                ),
                Command::Native
            );
        }
    }

    #[test]
    fn editing_preserves_native_keys_but_not_window_shortcuts() {
        assert_eq!(
            command(Key::Next, Modifiers::default(), true),
            Command::Native
        );
        assert_eq!(
            command(Key::Escape, Modifiers::default(), true),
            Command::Quit
        );
        assert_eq!(
            command(Key::Tab, Modifiers::default(), false),
            Command::ToggleMetadata
        );
        assert_eq!(
            command(Key::Tab, Modifiers::default(), true),
            Command::ToggleMetadata
        );
        assert_eq!(
            command(
                Key::Open,
                Modifiers {
                    primary: true,
                    shift: true,
                    ..Modifiers::default()
                },
                true
            ),
            Command::Open { folder: true }
        );
    }

    #[test]
    fn modified_reading_keys_do_not_steal_os_shortcuts() {
        assert_eq!(
            command(
                Key::Next,
                Modifiers {
                    alternate: true,
                    ..Modifiers::default()
                },
                false
            ),
            Command::Native
        );
        assert_eq!(
            command(
                Key::Rotate,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                false
            ),
            Command::Rotate { backwards: true }
        );
    }
}
