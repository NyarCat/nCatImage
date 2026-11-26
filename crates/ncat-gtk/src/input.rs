//! GDK key normalization only; command priority lives in ncat-presentation.
use gtk::gdk;
use ncat_presentation::input::Key;

pub fn key(key: gdk::Key) -> Key {
    match key {
        gdk::Key::comma => Key::FramePrevious,
        gdk::Key::period => Key::FrameNext,
        gdk::Key::bracketleft => Key::BookPrevious,
        gdk::Key::bracketright => Key::BookNext,
        gdk::Key::slash => Key::Playback,
        gdk::Key::o | gdk::Key::O => Key::Open,
        gdk::Key::l | gdk::Key::L => Key::Network,
        gdk::Key::q | gdk::Key::Q => Key::Quit,
        gdk::Key::F11 => Key::Fullscreen,
        gdk::Key::Escape => Key::Escape,
        gdk::Key::grave => Key::ToggleInterface,
        gdk::Key::Tab | gdk::Key::ISO_Left_Tab => Key::Tab,
        gdk::Key::space | gdk::Key::Page_Down | gdk::Key::Down | gdk::Key::KP_Down => Key::Next,
        gdk::Key::BackSpace | gdk::Key::Page_Up | gdk::Key::Up | gdk::Key::KP_Up => Key::Previous,
        gdk::Key::Left => Key::Left,
        gdk::Key::Right => Key::Right,
        gdk::Key::Home => Key::First,
        gdk::Key::End => Key::Last,
        gdk::Key::plus | gdk::Key::equal => Key::ZoomIn,
        gdk::Key::minus => Key::ZoomOut,
        gdk::Key::_9 => Key::Fit,
        gdk::Key::r | gdk::Key::R => Key::Rotate,
        gdk::Key::d => Key::Direction,
        gdk::Key::F5 => Key::Reload,
        gdk::Key::Delete | gdk::Key::KP_Delete => Key::Delete,
        _ => Key::Other,
    }
}
