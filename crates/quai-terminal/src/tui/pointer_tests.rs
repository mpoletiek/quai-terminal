//! The mouse against the real screens: what a click does, what it may never do.

use super::super::app::{App, ConfirmAction, Modal, Screen, Section};
use super::super::hit::{ListId, Target};
use super::super::ui::draw;
use super::super::ui::tests::populated_app;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};

const SIZE: (u16, u16) = (160, 48);

fn frame(app: &mut App, term: &mut Terminal<TestBackend>) {
    term.draw(|f| draw(f, app)).unwrap();
    // Tests click at once; the grace for a modal that just appeared is its own test.
    app.modal_since.set(Some(Instant::now() - Duration::from_secs(5)));
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }, SIZE);
}

fn click(app: &mut App, x: u16, y: u16) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), x, y);
}

fn centre(r: Rect) -> (u16, u16) {
    (r.x + r.width / 2, r.y + r.height / 2)
}

/// Where a target was drawn this frame (its first region).
fn find(app: &App, want: impl Fn(&Target) -> bool) -> Option<Rect> {
    app.hits.borrow().live().iter().find(|(_, t)| want(t)).map(|(r, _)| *r)
}

fn setup() -> (tempfile::TempDir, App, Terminal<TestBackend>) {
    let (dir, mut app) = populated_app();
    app.config.mouse = wallet_core::config::MouseMode::Full;
    let term = Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).unwrap();
    (dir, app, term)
}

/// Every section in the rail, every sub-tab, and the footer's hints do what their keys do.
#[test]
fn chrome_clicks_do_what_their_keys_do() {
    let (_dir, mut app, mut term) = setup();
    app.switch(Screen::Home);
    for section in [Section::Trade, Section::Nfts, Section::Activity, Section::System, Section::Home] {
        frame(&mut app, &mut term);
        let r = find(&app, |t| *t == Target::Section(section)).unwrap_or_else(|| panic!("{section:?} in the rail"));
        let (x, y) = centre(r);
        click(&mut app, x, y);
        assert_eq!(app.screen.section(), section, "clicking {section:?}");
    }
    app.switch(Screen::Markets);
    frame(&mut app, &mut term);
    let r = find(&app, |t| *t == Target::Tab(2)).expect("a third tab");
    let (x, y) = centre(r);
    click(&mut app, x, y);
    assert_eq!(app.screen, Section::Markets.screens(&app.config.features)[2], "the third tab, by click");
    // `?` in the footer opens the keys, as the key does.
    app.switch(Screen::Home);
    frame(&mut app, &mut term);
    let r = find(&app, |t| *t == Target::Key(KeyCode::Char('?'))).expect("? in the footer");
    let (x, y) = centre(r);
    click(&mut app, x, y);
    assert!(matches!(app.modal, Modal::Help));
    // And the backdrop closes what is only read.
    frame(&mut app, &mut term);
    click(&mut app, 0, SIZE.1 - 1);
    assert!(matches!(app.modal, Modal::None), "a click beside Help closes it");
}

/// A press and a release on different targets is no click; dragging off a button cancels it.
#[test]
fn a_click_is_press_and_release_on_the_same_thing() {
    let (_dir, mut app, mut term) = setup();
    app.switch(Screen::Home);
    frame(&mut app, &mut term);
    let trade = find(&app, |t| *t == Target::Section(Section::Trade)).unwrap();
    let nfts = find(&app, |t| *t == Target::Section(Section::Nfts)).unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), trade.x + 2, trade.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), nfts.x + 2, nfts.y);
    assert_eq!(app.screen, Screen::Home, "dragged off: nothing happened");
}

/// Rows select what was drawn under the pointer, and the window stays where it was: a row
/// clicked near the bottom of a scrolled list does not scroll the list out from under it.
#[test]
fn clicking_a_row_selects_it_without_scrolling() {
    let (_dir, mut app, mut term) = setup();
    for screen in [Screen::Activity, Screen::Accounts, Screen::Settings, Screen::Qi, Screen::Contacts, Screen::Wallets] {
        app.switch(screen);
        frame(&mut app, &mut term);
        let rows: Vec<(Rect, usize)> = app
            .hits
            .borrow()
            .live()
            .iter()
            .filter_map(|(r, t)| match t {
                Target::Row { list: ListId::Screen(s, 0), index, .. } if *s == screen => Some((*r, *index)),
                _ => None,
            })
            .collect();
        assert!(!rows.is_empty(), "{screen:?} registers its rows");
        let offset_before = app.view_offset();
        let (r, index) = *rows.last().unwrap();
        click(&mut app, r.x + 1, r.y);
        assert_eq!(app.selected, index, "{screen:?}: the clicked row is selected");
        frame(&mut app, &mut term);
        assert_eq!(app.view_offset(), offset_before, "{screen:?}: the list did not move under the pointer");
    }
}

/// The wheel scrolls the list under the pointer and leaves the selection; the keyboard brings
/// the selection back into view.
#[test]
fn the_wheel_scrolls_the_list_under_the_pointer() {
    let (_dir, mut app, mut term) = setup();
    app.switch(Screen::Settings);
    let small = Terminal::new(TestBackend::new(80, 24));
    let mut term_small = small.unwrap();
    frame(&mut app, &mut term_small);
    let r = find(&app, |t| matches!(t, Target::Row { list: ListId::Screen(Screen::Settings, 0), .. })).expect("settings rows");
    let before = app.view_offset();
    mouse(&mut app, MouseEventKind::ScrollDown, r.x + 1, r.y);
    frame(&mut app, &mut term_small);
    assert!(app.view_offset() > before, "the list scrolled");
    assert_eq!(app.selected, 0, "the selection stayed");
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), (80, 24));
    frame(&mut app, &mut term_small);
    let selected = app.selected;
    assert!(
        find(&app, |t| matches!(t, Target::Row { list: ListId::Screen(Screen::Settings, 0), index, .. } if *index == selected)).is_some(),
        "the keyboard brings the selection back into view"
    );
    let _ = &mut term;
}

/// No click, double-click or wheel on a review can sign it. Approve only arms, and the keyboard
/// is what signs.
#[test]
fn a_click_can_never_sign() {
    let (_dir, mut app, mut term) = setup();
    let reviews: Vec<Modal> = super::super::ui::tests::modals(&app).into_iter().filter(|m| matches!(m, Modal::Review(_))).collect();
    assert!(!reviews.is_empty());
    for review in reviews {
        let Modal::Review(r) = review else { unreachable!() };
        let open = |app: &mut App| {
            app.modal = Modal::Review(super::super::app::ReviewState {
                review: r.review.clone(),
                scroll: 0,
                content_lines: 0,
                viewport: 0,
                approve_focused: false,
                opened: Instant::now() - Duration::from_secs(60),
            });
        };
        open(&mut app);
        app.move_selection(10_000);
        frame(&mut app, &mut term);
        let regions: Vec<(Rect, Target)> = app.hits.borrow().live().to_vec();
        for (rect, target) in regions {
            let (x, y) = centre(rect);
            for _ in 0..2 {
                click(&mut app, x, y);
            }
            mouse(&mut app, MouseEventKind::ScrollDown, x, y);
            assert!(app.committing_kind.is_none(), "clicking {target:?} signed");
            if !matches!(app.modal, Modal::Review(_)) {
                // Reject (or the backdrop of something else) closed it: open it again.
                open(&mut app);
                app.move_selection(10_000);
            }
            frame(&mut app, &mut term);
        }
        // Approve by click arms it; Enter signs.
        let approve = find(&app, |t| *t == Target::Review(super::super::hit::ReviewPart::Approve)).expect("approve button");
        let (x, y) = centre(approve);
        click(&mut app, x, y);
        assert!(matches!(&app.modal, Modal::Review(r) if r.approve_focused), "armed");
        assert!(app.committing_kind.is_none(), "armed, not signed");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), SIZE);
        assert!(app.committing_kind.is_some(), "the keyboard signs");
        app.committing_kind = None;
    }
}

/// A click in the first instant after a modal appears was meant for what was there before.
#[test]
fn a_click_right_after_a_modal_opens_is_ignored() {
    let (_dir, mut app, mut term) = setup();
    app.switch(Screen::Home);
    app.modal = Modal::Confirm { title: "Quit".into(), body: "Leave?".into(), action: ConfirmAction::Quit };
    term.draw(|f| draw(f, &mut app)).unwrap();
    let yes = find(&app, |t| *t == Target::Confirm(true)).expect("yes");
    let (x, y) = centre(yes);
    click(&mut app, x, y);
    assert!(!app.quit && matches!(app.modal, Modal::Confirm { .. }), "too soon: ignored");
    app.modal_since.set(Some(Instant::now() - Duration::from_secs(1)));
    click(&mut app, x, y);
    assert!(app.quit, "after the grace, yes is yes");
}

/// Accepting a stranger's payment channel is armed by a click, finished by the key.
#[test]
fn a_sensitive_confirmation_only_arms() {
    let (_dir, mut app, mut term) = setup();
    let (worker, mut sent) = super::super::worker::Worker::capture();
    app.worker = Some(worker);
    app.modal = Modal::Confirm { title: "Accept".into(), body: "Accept?".into(), action: ConfirmAction::AcceptOffer("PM8T".into()) };
    frame(&mut app, &mut term);
    let yes = find(&app, |t| *t == Target::Confirm(true)).expect("yes");
    let (x, y) = centre(yes);
    click(&mut app, x, y);
    assert!(matches!(app.modal, Modal::Confirm { .. }), "still asking");
    assert!(sent.try_recv().is_err(), "nothing was sent");
    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), SIZE);
    assert!(sent.try_recv().is_ok(), "the key accepts");
}

/// A stray click beside a form must not throw away what was typed.
#[test]
fn the_backdrop_keeps_a_form_open() {
    let (_dir, mut app, mut term) = setup();
    app.switch(Screen::Home);
    app.open_form(super::super::app::FormKind::SendQuai);
    assert!(matches!(app.modal, Modal::Form(_)));
    frame(&mut app, &mut term);
    click(&mut app, 0, SIZE.1 - 1);
    assert!(matches!(app.modal, Modal::Form(_)), "the form is still open");
    // A field is focused by clicking it.
    let second = find(&app, |t| *t == Target::Field(1)).expect("a second field");
    let (x, y) = centre(second);
    click(&mut app, x, y);
    assert!(matches!(&app.modal, Modal::Form(f) if f.focus == 1));
}

/// Nothing behind a modal answers a click, and every region lies inside the frame.
#[test]
fn a_modal_captures_the_pointer_and_regions_stay_on_screen() {
    let (_dir, mut app, mut term) = setup();
    for m in super::super::ui::tests::modals(&app).into_iter().filter(|m| !matches!(m, Modal::None)) {
        app.switch(Screen::Home);
        let before = super::super::app::modal_name(&m);
        app.modal = m;
        frame(&mut app, &mut term);
        let hits = app.hits.borrow();
        assert!(hits.captured(), "{before} took the pointer (now {})", super::super::app::modal_name(&app.modal));
        for (r, t) in hits.live() {
            assert!(r.right() <= SIZE.0 && r.bottom() <= SIZE.1, "{t:?} at {r:?} is off screen");
            assert!(
                !matches!(t, Target::Section(_) | Target::Tab(_) | Target::Key(_) | Target::Pane(_)),
                "{t:?} answers through the modal"
            );
        }
    }
}

/// The pointer's only way to press a key refuses everything but Esc on an open review, and yes
/// on a confirmation only the keyboard may finish.
#[test]
fn the_pointer_cannot_press_a_signing_key() {
    let (_dir, mut app, mut term) = setup();
    let review = super::super::ui::tests::modals(&app).into_iter().find(|m| matches!(m, Modal::Review(_))).expect("a review");
    let Modal::Review(r) = review else { unreachable!() };
    app.modal = Modal::Review(super::super::app::ReviewState {
        review: r.review.clone(),
        scroll: 0,
        content_lines: 0,
        viewport: 0,
        approve_focused: true,
        opened: Instant::now() - Duration::from_secs(60),
    });
    app.move_selection(10_000);
    frame(&mut app, &mut term);
    assert!(matches!(&app.modal, Modal::Review(r) if r.can_approve() && r.approve_focused), "armed and read: Enter would sign");
    for code in [KeyCode::Enter, KeyCode::Char('y'), KeyCode::Tab, KeyCode::Char(' ')] {
        app.press(code, SIZE);
        assert!(app.committing_kind.is_none() && matches!(app.modal, Modal::Review(_)), "{code:?} got through");
    }
}

/// What is clickable on every screen, as a picture: each region's cells marked by what it does,
/// later regions over earlier ones, as on screen. Checked in under `golden/hits`, so a change to
/// what a click does shows up in review the way a change to what is drawn does. `QW_BLESS=1`
/// rewrites them.
#[test]
fn hit_maps_match_their_golden_files() {
    let (_dir, mut app, _) = setup();
    let (w, h) = (100u16, 30u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui/golden/hits");
    let bless = std::env::var("QW_BLESS").is_ok();
    let mut drift = Vec::new();
    let mark = |t: &Target| -> char {
        match t {
            Target::Section(_) => 'S',
            Target::Tab(_) => 'T',
            Target::Key(_) => 'K',
            Target::Pane(_) => '·',
            Target::Row { index, .. } => char::from_digit((*index % 10) as u32, 10).unwrap_or('#'),
            Target::Scroll(_) => '~',
            Target::Header(_) => 'H',
            Target::Toast => '!',
            Target::Review(super::super::hit::ReviewPart::Reject) => 'R',
            Target::Review(super::super::hit::ReviewPart::Approve) => 'A',
            Target::Confirm(true) => 'Y',
            Target::Confirm(false) => 'N',
            Target::Field(_) => 'F',
            Target::CardField(_) => 'f',
            Target::Choice { .. } => 'C',
            Target::Button(_) => 'B',
            Target::Swallow => '.',
            Target::Route(_) => 'g',
            Target::Backdrop => ' ',
        }
    };
    let picture = |app: &App| -> String {
        let mut grid = vec![vec![' '; w as usize]; h as usize];
        for (r, t) in app.hits.borrow().live() {
            for y in r.y..r.bottom().min(h) {
                for x in r.x..r.right().min(w) {
                    grid[y as usize][x as usize] = mark(t);
                }
            }
        }
        grid.into_iter().map(|row| row.into_iter().collect::<String>().trim_end().to_string() + "\n").collect()
    };
    let mut check = |name: String, out: String| {
        let file = golden.join(format!("{name}.txt"));
        if bless {
            std::fs::create_dir_all(&golden).unwrap();
            std::fs::write(&file, &out).unwrap();
        } else if std::fs::read_to_string(&file).ok().as_deref() != Some(out.as_str()) {
            std::fs::write(file.with_extension("actual"), &out).unwrap();
            drift.push(file.display().to_string());
        }
    };
    for screen in Screen::ALL {
        if screen == Screen::DataSources {
            continue;
        }
        app.switch(screen);
        frame(&mut app, &mut term);
        check(format!("{screen:?}_{w}x{h}"), picture(&app));
    }
    for (i, m) in super::super::ui::tests::modals(&app).into_iter().filter(|m| !matches!(m, Modal::None)).enumerate() {
        app.switch(Screen::Home);
        let name = super::super::app::modal_name(&m).replace(' ', "_");
        app.modal = m;
        frame(&mut app, &mut term);
        check(format!("modal{i:02}_{name}_{w}x{h}"), picture(&app));
    }
    assert!(drift.is_empty(), "hit maps differ from their golden files (QW_BLESS=1 to accept):\n{}", drift.join("\n"));
}

/// Right-click focuses the row under the pointer and opens its actions: the same sheet space
/// opens. One click on an item runs it.
#[test]
fn right_click_opens_the_rows_actions() {
    let (_dir, mut app, mut term) = setup();
    app.switch(Screen::Contacts);
    frame(&mut app, &mut term);
    let rows: Vec<(Rect, usize)> = app
        .hits
        .borrow()
        .live()
        .iter()
        .filter_map(|(r, t)| match t {
            Target::Row { list: ListId::Screen(Screen::Contacts, 0), index, .. } => Some((*r, *index)),
            _ => None,
        })
        .collect();
    let (r, index) = *rows.last().expect("contact rows");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Right), r.x + 1, r.y);
    assert_eq!(app.selected, index, "the row under the pointer is the focus");
    assert!(matches!(app.modal, Modal::Sheet { .. }), "its actions opened");
    frame(&mut app, &mut term);
    let n = match &app.modal {
        Modal::Sheet { items, .. } => items.len(),
        _ => 0,
    };
    let drawn = app.hits.borrow().live().iter().filter(|(_, t)| matches!(t, Target::Row { list: ListId::Sheet, .. })).count();
    assert_eq!(drawn, n, "every action is on screen and clickable");
    let edit = app
        .hits
        .borrow()
        .live()
        .iter()
        .find(|(_, t)| matches!(t, Target::Row { list: ListId::Sheet, .. }))
        .map(|(r, _)| *r)
        .expect("sheet rows");
    let (x, y) = centre(edit);
    click(&mut app, x, y);
    assert!(!matches!(app.modal, Modal::Sheet { .. }), "the item ran and the sheet closed");
}

/// A card's fields take a click the way they take Tab: Swap, Convert and Wrap each focus the
/// field under the pointer, and no click on a card signs anything.
#[test]
fn card_fields_take_a_click() {
    let (_dir, mut app, mut term) = setup();
    for (screen, field, get) in [
        (Screen::Swap, 3usize, (|a: &App| a.eco.swap.field) as fn(&App) -> usize),
        (Screen::Convert, 2, |a: &App| a.eco.convert.field),
        (Screen::Wrap, 0, |a: &App| a.eco.wrap.field),
    ] {
        app.switch(screen);
        frame(&mut app, &mut term);
        let r = find(&app, |t| *t == Target::CardField(field)).unwrap_or_else(|| panic!("{screen:?} field {field} is clickable"));
        let (x, y) = centre(r);
        click(&mut app, x, y);
        assert_eq!(get(&app), field, "{screen:?}: the click focused field {field}");
        assert!(!matches!(app.modal, Modal::Review(_)), "{screen:?}: a click on a field opened a review");
    }
}

/// The pointer says what a click would do before it is made: a hand on what a click acts on,
/// a text cursor on fields, not-allowed on an Approve that is not yet read, and the terminal's
/// own pointer elsewhere.
#[test]
fn the_pointer_takes_the_shape_of_what_is_under_it() {
    use super::super::hit::ReviewPart;
    let (_dir, mut app, _term) = setup();
    app.pointer.hover = None;
    assert_eq!(app.pointer_shape(), "default");
    app.pointer.hover = Some(Target::Row { list: ListId::Screen(Screen::Home, 0), index: 0, key: None });
    assert_eq!(app.pointer_shape(), "pointer");
    app.pointer.hover = Some(Target::CardField(0));
    assert_eq!(app.pointer_shape(), "text");
    app.pointer.hover = Some(Target::Swallow);
    assert_eq!(app.pointer_shape(), "default", "inside a modal where nothing is clickable");
    let review = super::super::ui::tests::modals(&app)
        .into_iter()
        .find_map(|m| if let Modal::Review(r) = m { Some(r.review) } else { None })
        .expect("a review");
    let state = |scroll: u16, age: u64| super::super::app::ReviewState {
        review: review.clone(),
        scroll,
        content_lines: 40,
        viewport: 10,
        approve_focused: false,
        opened: Instant::now() - Duration::from_secs(age),
    };
    app.pointer.hover = Some(Target::Review(ReviewPart::Approve));
    app.modal = Modal::Review(state(0, 60));
    assert_eq!(app.pointer_shape(), "not-allowed", "not read to the end");
    app.modal = Modal::Review(state(30, 60));
    assert_eq!(app.pointer_shape(), "pointer", "read: a click arms it (and only arms it)");
}
