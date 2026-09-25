use super::*;

#[test]
fn every_collected_tile_gets_its_picture() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::explorer::{NftItem, TokenKind};
    use wallet_core::market::OwnedNft;
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let meta = registry.create_watch("p", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    let mut caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Pixels);
    caps.tier = Tier::Pixels;
    let theme = super::super::theme::resolve(paths.root(), "quai-red", false, false).0;
    let connected = wallet_core::config::AppConfig {
        explorer_lookups: true,
        images: true,
        token_icons: true,
        features: wallet_core::config::Features { messaging: true, trading: true, nfts: true },
        ..Default::default()
    };
    let mut app = App::new(paths, "local".into(), connected, theme, caps, Some(meta));
    app.locked = false;
    app.onboarding = None;
    app.config.motion = Motion::Off;
    let mut items = Vec::new();
    for (i, color) in [(10u8, 200u8, 10u8), (200, 10, 10), (10, 10, 200)].into_iter().enumerate() {
        let url = format!("https://example.test/{i}.png");
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, 64, 64);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let px: Vec<u8> = (0..64 * 64).flat_map(|_| [color.0, color.1, color.2, 255]).collect();
            enc.write_header().unwrap().write_image_data(&px).unwrap();
        }
        let r = std::sync::Arc::new(wallet_core::media::trusted_rendition(&png, wallet_core::media::THUMB).unwrap());
        app.eco.images.insert((url.clone(), wallet_core::media::THUMB), super::super::eco::ImageSlot::Ready(r, std::time::Instant::now()));
        let item = NftItem {
            contract: format!("0x00{i}"),
            token_id: i.to_string(),
            name: format!("item {i}"),
            image: Some(url),
            ..NftItem::default()
        };
        items.push(OwnedNft { item, owner: "0x00".into(), kind: TokenKind::Erc721, quantity: "1".into(), verified: true });
    }
    app.eco.nfts = Some(Ok(items));
    app.switch(Screen::Collected);
    let mut term = Terminal::new(TestBackend::new(160, 48)).unwrap();
    // Pictures are fitted and encoded off the UI thread: the first frame reserves their cells, and
    // they are placed on the frame after their encode lands.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let tiles = loop {
        super::super::images::poll_fitted(&app);
        term.draw(|f| draw(f, &mut app)).unwrap();
        let placed = super::super::images::kitty_items(&app);
        let tiles: Vec<_> = placed.iter().filter(|p| p.rows > 2).map(|p| (p.x, p.y, p.cols, p.rows)).collect();
        if tiles.len() == 3 || std::time::Instant::now() > deadline {
            break tiles;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(tiles.len(), 3, "placements: {tiles:?}");
}
