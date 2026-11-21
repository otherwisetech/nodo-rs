//! Publishing devices and sites to the shared registry, on temp dirs only.
use nodo_core::db::Store;
use nodo_core::model::{Device, Ident, IdentKind};
use nodo_core::sync::{self, DETACHED};
use otherwise_link::entity::{Company, Entity as Card, Kind};
use otherwise_link::registry::Registry;
use otherwise_link::settings::HubSettings;
use otherwise_link::Root;

fn dev(id: i64, name: &str, mac: &str) -> Device {
    Device { id, name: name.into(), saved: true, idents: vec![Ident::new(IdentKind::Mac, mac)], location: "Oficina".into(), ..Default::default() }
}

#[test]
fn off_until_the_user_turns_it_on() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("m.nodo")).unwrap();
    let root = Root::at(dir.path().join("shared"));
    assert_eq!(sync::sync_shared(&mut s, &Default::default(), &root).unwrap(), None);
    assert!(!root.entities_db().exists());
    HubSettings::update(&root, |h| h.entities = true).unwrap();
    assert!(sync::sync_shared(&mut s, &Default::default(), &root).unwrap().is_some());
}

#[test]
fn devices_and_sites_published_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::open(&dir.path().join("entities.db")).unwrap();
    let mut s = Store::create(&dir.path().join("m.nodo")).unwrap();
    let mut data = nodo_core::db::MapData { devices: vec![dev(1, "NAS", "aa:bb:cc:00:00:01"), dev(2, "Printer", "aa:bb:cc:00:00:02"), Device { id: 3, name: "guest phone".into(), ..Default::default() }], ..Default::default() };
    data.devices[0].owner = "otherwise s.a.s.".into();
    // the owner is a company another app shared
    let mut co = Card::new(Kind::Company, "Otherwise S.A.S.").with_ref("vigia", "entity-1@x");
    co.company = Some(Company::default());
    let co = reg.upsert("vigia", co).unwrap().id;
    let r = sync::sync(&mut s, &data, &mut reg).unwrap();
    assert_eq!(r.pushed, 3, "two saved devices and one site; never the unsaved one");
    assert_eq!(reg.counts().unwrap().get(&Kind::Device), Some(&2));
    assert_eq!(reg.counts().unwrap().get(&Kind::Site), Some(&1));
    let nas = reg.by_key("mac:aa:bb:cc:00:00:01").unwrap().unwrap();
    assert_eq!(nas.ext["nodo"]["ownerEntity"], serde_json::json!(co.to_string()));
    assert_eq!(sync::owner_card(&reg, "OTHERWISE SAS").map(|c| c.id), None, "no fuzzy guesses");
    assert_eq!(sync::owner_card(&reg, "Otherwise S.A.S.").map(|c| c.id), Some(co));
    // nothing changed: nothing pushed
    assert_eq!(sync::sync(&mut s, &data, &mut reg).unwrap().pushed, 0);
    // a rename goes out; the card keeps its id
    data.devices[0].name = "NAS Synology".into();
    assert_eq!(sync::sync(&mut s, &data, &mut reg).unwrap().pushed, 1);
    assert_eq!(reg.get(nas.id).unwrap().unwrap().name, "NAS Synology");
    assert_eq!(sync::device_for_card(&s, &reg, nas.id), Some(1));
    // the printer leaves the inventory: its ref is unlinked, the card stays
    data.devices[1].saved = false;
    let printer = reg.by_key("mac:aa:bb:cc:00:00:02").unwrap().unwrap().id;
    assert_eq!(sync::sync(&mut s, &data, &mut reg).unwrap().unlinked, 1);
    assert!(!reg.get(printer).unwrap().unwrap().refs.contains_key("nodo"));
}

#[test]
fn merges_and_deletes_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::open(&dir.path().join("entities.db")).unwrap();
    let mut s = Store::create(&dir.path().join("m.nodo")).unwrap();
    let mut data = nodo_core::db::MapData { devices: vec![dev(1, "NAS", "aa:bb:cc:00:00:01")], ..Default::default() };
    data.devices[0].location.clear();
    sync::sync(&mut s, &data, &mut reg).unwrap();
    let nas = reg.by_key("mac:aa:bb:cc:00:00:01").unwrap().unwrap().id;
    let other = reg.upsert("otherwise-apps", Card::new(Kind::Device, "Synology")).unwrap().id;
    reg.merge("otherwise-apps", other, nas).unwrap();
    sync::sync(&mut s, &data, &mut reg).unwrap();
    assert_eq!(s.shared_links().unwrap()[0].shared_id, other.to_string());
    assert_eq!(sync::device_for_card(&s, &reg, nas), Some(1), "the old id still finds the device");
    reg.delete("otherwise-apps", other).unwrap();
    sync::sync(&mut s, &data, &mut reg).unwrap();
    assert_eq!(s.shared_links().unwrap()[0].shared_id, DETACHED);
    data.devices[0].name = "NAS 2".into();
    assert_eq!(sync::sync(&mut s, &data, &mut reg).unwrap().pushed, 0, "detached for good");
}

#[test]
fn two_maps_of_one_network_share_a_card_without_fighting() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::open(&dir.path().join("entities.db")).unwrap();
    let mut a = Store::create(&dir.path().join("a.nodo")).unwrap();
    let mut b = Store::create(&dir.path().join("b.nodo")).unwrap();
    let mut da = nodo_core::db::MapData { devices: vec![dev(1, "Router", "aa:bb:cc:00:00:09")], ..Default::default() };
    let mut db = nodo_core::db::MapData { devices: vec![dev(5, "Gateway", "aa:bb:cc:00:00:09")], ..Default::default() };
    sync::sync(&mut a, &da, &mut reg).unwrap();
    sync::sync(&mut b, &db, &mut reg).unwrap();
    assert_eq!(reg.counts().unwrap().get(&Kind::Device), Some(&1), "same MAC, same card");
    let card = reg.by_key("mac:aa:bb:cc:00:00:09").unwrap().unwrap();
    let r = card.refs["nodo"].clone();
    db.devices[0].name = "Gateway 2".into();
    sync::sync(&mut b, &db, &mut reg).unwrap();
    da.devices[0].name = "Router 2".into();
    sync::sync(&mut a, &da, &mut reg).unwrap();
    sync::sync(&mut b, &db, &mut reg).unwrap();
    assert_eq!(reg.get(card.id).unwrap().unwrap().refs["nodo"], r, "the ref stays with whoever has it");
}
