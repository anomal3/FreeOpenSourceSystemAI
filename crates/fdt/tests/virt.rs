//! Разбор **настоящего** дерева устройств.
//!
//! Дерево снято с машины `virt` QEMU (`-machine virt,dumpdtb=...`) и лежит
//! рядом файлом. Проверять разборщик на дереве, которое сами же и собрали, — это
//! проверять, что мы одинаково понимаем свой формат; здесь же читается то, что
//! написал чужой код, ровно как на телефоне будет читаться то, что написал LK.
//!
//! Того же дерева, что в телефоне, у нас нет: `/sys/firmware/fdt` закрыт
//! SELinux, а root'а на аппарате нет. Но формат один, а узлы, которые ядру
//! нужны (`/memory`, GIC, таймер, `/chosen`), у `virt` и у MT676x называются
//! одинаково — это и проверяется.

use fdt::Fdt;

const BLOB: &[u8] = include_bytes!("virt.dtb");

#[test]
fn a_real_tree_parses_and_a_broken_one_does_not() {
    assert!(Fdt::new(BLOB).is_some());
    assert!(Fdt::new(&BLOB[..39]).is_none(), "обрезанный заголовок");
    assert!(Fdt::new(b"not a device tree at all").is_none());

    // Испорченная метка — не дерево, даже если всё остальное на месте.
    let mut spoiled = BLOB.to_vec();
    spoiled[0] ^= 0xff;
    assert!(Fdt::new(&spoiled).is_none(), "чужая метка");

    // Заявленный размер больше того, что дали, — отказ, а не чтение за концом.
    let mut lying = BLOB.to_vec();
    lying[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(Fdt::new(&lying).is_none(), "размер за пределами среза");
}

#[test]
fn the_root_and_its_children_are_where_they_should_be() {
    let fdt = Fdt::new(BLOB).unwrap();
    let root = fdt.nodes().next().unwrap();
    assert_eq!(root.depth, 0);
    assert_eq!(root.name, "");

    // У корня всегда есть размеры ячеек — без них не прочитать ни один `reg`.
    assert_eq!(root.property_u64("#address-cells"), Some(2));
    assert_eq!(root.property_u64("#size-cells"), Some(2));

    let names: Vec<&str> = fdt.nodes().filter(|n| n.depth == 1).map(|n| n.name).collect();
    assert!(names.iter().any(|n| n.starts_with("memory")), "{names:?}");
    assert!(names.iter().any(|n| *n == "chosen"), "{names:?}");
}

#[test]
fn memory_says_how_much_ram_the_machine_has() {
    let fdt = Fdt::new(BLOB).unwrap();
    let memory = fdt.find("/memory").expect("узел памяти");
    assert_eq!(memory.property_str("device_type"), Some("memory"));

    // Машина запускалась с `-m 512`, и это ровно то, что дерево обязано сказать.
    let total: u64 = memory.reg(2, 2).map(|region| region.size).sum();
    assert_eq!(total, 512 * 1024 * 1024, "объём памяти из дерева");

    let first = memory.reg(2, 2).next().unwrap();
    assert_eq!(first.address, 0x4000_0000, "ОЗУ `virt` начинается здесь");
}

#[test]
fn the_interrupt_controller_is_found_by_compatibility_not_by_name() {
    let fdt = Fdt::new(BLOB).unwrap();
    // Имя узла у GIC — `intc@8000000`, и искать его по имени значило бы знать
    // адрес заранее. По совместимости — не значит.
    let gic = fdt
        .find_compatible("arm,cortex-a15-gic")
        .or_else(|| fdt.find_compatible("arm,gic-v3"))
        .expect("контроллер прерываний");
    let distributor = gic.reg(2, 2).next().expect("первая область GIC");
    assert_ne!(distributor.address, 0);
    assert!(gic.property("interrupt-controller").is_some());
}

#[test]
fn the_timer_declares_the_generic_counter() {
    let fdt = Fdt::new(BLOB).unwrap();
    let timer = fdt.find_compatible("arm,armv8-timer").expect("таймер");
    // Прерывания у таймера — четыре тройки: тип, номер, флаги.
    assert_eq!(timer.property("interrupts").map(<[u8]>::len), Some(4 * 3 * 4));
}

#[test]
fn chosen_carries_what_the_bootloader_decided() {
    let fdt = Fdt::new(BLOB).unwrap();
    let chosen = fdt.find("/chosen").expect("узел chosen");
    // У `virt` это `stdout-path`; у телефона в этом же узле лежит адрес
    // кадрового буфера (`atag,videolfb-fb_base_*`). Важно, что узел находится и
    // читается — свойства в нём у каждой машины свои.
    assert!(
        chosen.properties().count() > 0,
        "в chosen обязано что-то быть"
    );
}

#[test]
fn a_name_matches_with_and_without_its_address() {
    let fdt = Fdt::new(BLOB).unwrap();
    // Один и тот же узел находится и по короткому имени, и по полному.
    let short = fdt.find("/memory").unwrap();
    let full = fdt.find(&format!("/{}", short.name)).unwrap();
    assert_eq!(short.name, full.name);
    assert_eq!(short.depth, full.depth);
}

#[test]
fn compatible_is_a_list_and_every_entry_counts() {
    let fdt = Fdt::new(BLOB).unwrap();
    let root = fdt.nodes().next().unwrap();
    let list: Vec<&str> = root.strings("compatible").collect();
    assert!(!list.is_empty(), "у корня всегда есть compatible");
    assert!(list.iter().all(|value| !value.is_empty()));
    assert!(root.is_compatible(list[0]));
    assert!(!root.is_compatible("something,else"));
}

#[test]
fn nested_nodes_keep_their_depth() {
    let fdt = Fdt::new(BLOB).unwrap();
    // В дереве `virt` есть вложенность: у корня — `cpus`, у него — `cpu@0`.
    let deep = fdt.nodes().filter(|node| node.depth >= 2).count();
    assert!(deep > 0, "вложенные узлы обязаны находиться");

    // Глубина не убегает: она растёт на BEGIN_NODE и падает на END_NODE, и в
    // корректном дереве никогда не уходит в минус — иначе обход оборвался бы.
    let all = fdt.nodes().count();
    assert!(all > deep, "узлов всего больше, чем вложенных");
}

#[test]
fn reg_refuses_cell_counts_it_cannot_represent() {
    let fdt = Fdt::new(BLOB).unwrap();
    let memory = fdt.find("/memory").unwrap();
    // Ноль ячеек и больше двух — не «прочитаем что получится», а отказ: адрес,
    // собранный из чужого числа ячеек, правдоподобен и неверен.
    assert_eq!(memory.reg(0, 2).count(), 0);
    assert_eq!(memory.reg(3, 2).count(), 0);
    assert!(memory.reg(2, 2).count() > 0);
}
