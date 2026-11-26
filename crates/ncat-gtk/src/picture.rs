//! GTK page surface with native-size measurement independent of texture resolution.
use gtk::{gdk, glib, graphene, prelude::*, subclass::prelude::*};
use ncat_presentation::ReaderView;

// GDK 4.12 added fractional surface scale. Keep GTK 4.10 compatibility by
// checking for the property at runtime; scale-factor rounds up on Wayland.
pub(crate) fn surface_scale(surface: &gdk::Surface) -> f64 {
    fractional_scale(surface).unwrap_or_else(|| f64::from(surface.scale_factor().max(1)))
}

fn fractional_scale(surface: &gdk::Surface) -> Option<f64> {
    surface.find_property("scale")?;
    let scale = surface.property::<f64>("scale");
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Picture {
        pub texture: RefCell<Option<gdk::MemoryTexture>>,
        pub view: Cell<ReaderView>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Picture {
        const NAME: &'static str = "NcatPagePicture";
        type Type = super::Picture;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Picture {}

    impl WidgetImpl for Picture {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let view = self.view.get();
            let size = if view.fit || self.texture.borrow().is_none() {
                1
            } else {
                let (width, height) = view.picture_size((1, 1), self.obj().device_scale());
                if orientation == gtk::Orientation::Horizontal {
                    width
                } else {
                    height
                }
            };
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let texture = self.texture.borrow();
            let Some(texture) = texture.as_ref() else {
                return;
            };
            let viewport = (obj.width(), obj.height());
            let view = self.view.get();
            let (width, height) = view.picture_size(viewport, obj.device_scale());
            let (x, y) = view.picture_origin((width, height), viewport, (0, 0));
            snapshot.append_texture(
                texture,
                &graphene::Rect::new(x as f32, y as f32, width as f32, height as f32),
            );
        }
    }
}

glib::wrapper! {
    pub struct Picture(ObjectSubclass<imp::Picture>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Picture {
    fn device_scale(&self) -> f64 {
        self.native()
            .and_then(|native| native.surface())
            .as_ref()
            .map_or(1.0, surface_scale)
    }

    pub fn watch_surface_scale(&self, surface: &gdk::Surface) {
        if surface.find_property("scale").is_some() {
            let weak = self.downgrade();
            surface.connect_notify_local(Some("scale"), move |_, _| {
                if let Some(picture) = weak.upgrade() {
                    picture.queue_resize();
                    picture.queue_draw();
                }
            });
        }
    }

    pub fn new() -> Self {
        let picture: Self = glib::Object::new();
        picture.set_focusable(true);
        picture.update_property(&[gtk::accessible::Property::Label("현재 만화 페이지")]);
        picture.connect_scale_factor_notify(|picture| picture.queue_resize());
        picture
    }

    pub fn texture(&self) -> Option<gdk::MemoryTexture> {
        self.imp().texture.borrow().clone()
    }

    pub fn set_texture(&self, texture: Option<gdk::MemoryTexture>) {
        self.imp().texture.replace(texture);
        self.queue_resize();
        self.queue_draw();
    }

    pub fn set_view(&self, view: ReaderView) {
        self.imp().view.set(view);
        let align = if view.fit {
            gtk::Align::Fill
        } else {
            // GtkViewport allocates at least the page's minimum size. Center
            // only within leftover space; an overflowing axis has none.
            gtk::Align::Center
        };
        self.set_halign(align);
        self.set_valign(align);
        self.queue_resize();
        self.queue_draw();
    }
}
