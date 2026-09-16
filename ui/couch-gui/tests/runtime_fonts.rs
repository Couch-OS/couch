use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::{ComponentHandle, Rgb8Pixel};

// Use the production font resources. A separate .slint test fixture containing
// Cyrillic literals would embed its own glyphs and hide the original bug.
slint::include_modules!();

fn render(window: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); 480 * 800];
    window.request_redraw();
    let mut rendered = false;
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 480);
        rendered = true;
    });
    assert!(rendered);
    pixels
}

fn text_region(pixels: &[Rgb8Pixel], bold: bool) -> Vec<Rgb8Pixel> {
    let (xs, ys) = if bold {
        (24..456, 300..400) // The dock clock uses Lato Bold at 62px.
    } else {
        (16..100, 10..46) // The status clock uses Lato Regular at 19px.
    };
    ys.flat_map(|y| pixels[y * 480 + xs.start..y * 480 + xs.end].iter().copied())
        .collect()
}

#[test]
fn runtime_cyrillic_and_symbols_render_in_both_font_weights() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    struct Platform(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for Platform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }

        fn duration_since_start(&self) -> Duration {
            Duration::ZERO
        }
    }
    slint::platform::set_platform(Box::new(Platform(window.clone()))).unwrap();
    window.set_size(slint::PhysicalSize::new(480, 800));
    let app = App::new().unwrap();
    app.show().unwrap();

    for bold in [false, true] {
        app.set_dock_clock_shown(bold);
        app.set_clock("".into());
        let blank = text_region(&render(&window), bold);

        // An unsupported character must remain blank, so an unrelated repaint
        // or a change to the crop cannot make every character pass this test.
        app.set_clock("\u{10ffff}".into());
        assert_eq!(text_region(&render(&window), bold), blank);

        // Russian alphabet plus the Ukrainian and Belarusian additions. These
        // strings originate in Rust, just like Home Assistant's runtime names.
        for character in ('А'..='я').chain("ЁёҐґЄєІіЇїЎў0123456789°C–—`".chars())
        {
            app.set_clock(character.to_string().into());
            let visible = text_region(&render(&window), bold)
                .iter()
                .zip(&blank)
                .any(|(actual, empty)| actual != empty);
            assert!(
                visible,
                "runtime character {character:?} is blank in {}",
                if bold { "Lato Bold" } else { "Lato Regular" }
            );
        }
    }
}
