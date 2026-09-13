//! Сборка узлов дерева: лист и внутренний узел.
//!
//! Один код на двоих — на `mkfs` и на писателя. Лист, собранный двумя разными
//! функциями, проверялся бы вдвое реже, а ошибка в одной из них была бы видна
//! только на томах, созданных именно ею.

use alloc::vec::Vec;

use crate::crc32c;
use crate::layout::*;
use crate::{Error, Result, try_zeroed};

/// Что одинаково у всех узлов одной транзакции.
pub(crate) struct Stamp {
    pub nodesize: u32,
    pub fsid: [u8; 16],
    pub chunk_uuid: [u8; 16],
    pub generation: u64,
}

/// Сколько указателей помещается во внутренний узел.
pub(crate) fn pointers_per_node(nodesize: u32) -> usize {
    (nodesize as usize - HEADER_SIZE) / KEY_PTR_SIZE
}

/// Сколько байт листа отдано под описания и данные элементов.
pub(crate) fn leaf_capacity(nodesize: u32) -> usize {
    nodesize as usize - HEADER_SIZE
}

/// Записать контрольную сумму блока в его первые байты.
pub(crate) fn seal(block: &mut [u8]) {
    let sum = crc32c::checksum(&block[CSUM_SIZE..]);
    block[..CSUM_SIZE].fill(0);
    put_u32(block, 0, sum);
}

/// Уложить элементы в лист и запечатать его.
///
/// Элементы обязаны идти строго по возрастанию ключа: это условие двоичного
/// поиска, а совпавшие ключи спрятали бы один из элементов навсегда.
///
/// Данные кладутся от конца узла навстречу таблице и **вплотную** друг к
/// другу: проверка дерева в ядре Linux требует, чтобы конец данных элемента
/// совпадал с началом данных предыдущего. Промежуток наш читатель пережил бы,
/// а ядро — нет.
pub(crate) fn leaf(
    items: &[(Key, Vec<u8>)],
    address: u64,
    owner: u64,
    stamp: &Stamp,
) -> Result<Vec<u8>> {
    if items.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(Error::Corrupt);
    }
    let nodesize = stamp.nodesize as usize;
    let mut node = try_zeroed(nodesize)?;
    let table = items.len().checked_mul(ITEM_SIZE).ok_or(Error::Unsupported)?;
    // Смещение данных считается от конца заголовка.
    let mut end = nodesize - HEADER_SIZE;
    for (index, (key, data)) in items.iter().enumerate() {
        // Сюда может привести только ошибка раскладки по листьям, и она обязана
        // стать отказом, а не узлом, в котором данные налезли на таблицу.
        if data.len() > end || end - data.len() < table {
            return Err(Error::Unsupported);
        }
        end -= data.len();
        let at = HEADER_SIZE + index * ITEM_SIZE;
        key.store(&mut node, at);
        put_u32(&mut node, at + KEY_SIZE, end as u32);
        put_u32(&mut node, at + KEY_SIZE + 4, data.len() as u32);
        node[HEADER_SIZE + end..HEADER_SIZE + end + data.len()].copy_from_slice(data);
    }
    header(&mut node, address, owner, items.len(), 0, stamp);
    seal(&mut node);
    Ok(node)
}

/// Внутренний узел: первый ключ каждого потомка и его адрес.
///
/// Поколение потомка берётся из печати транзакции: внутренний узел
/// перестраивается только вместе со всеми своими потомками.
pub(crate) fn internal(
    children: &[(Key, u64)],
    address: u64,
    owner: u64,
    level: u8,
    stamp: &Stamp,
) -> Result<Vec<u8>> {
    if level == 0
        || children.is_empty()
        || children.len() > pointers_per_node(stamp.nodesize)
        || children.windows(2).any(|pair| pair[0].0 >= pair[1].0)
    {
        return Err(Error::Corrupt);
    }
    let mut node = try_zeroed(stamp.nodesize as usize)?;
    for (index, (key, child)) in children.iter().enumerate() {
        let at = HEADER_SIZE + index * KEY_PTR_SIZE;
        key.store(&mut node, at);
        put_u64(&mut node, at + KEY_SIZE, *child);
        put_u64(&mut node, at + KEY_SIZE + 8, stamp.generation);
    }
    header(&mut node, address, owner, children.len(), level, stamp);
    seal(&mut node);
    Ok(node)
}

fn header(node: &mut [u8], address: u64, owner: u64, count: usize, level: u8, stamp: &Stamp) {
    node[HDR_FSID..HDR_FSID + 16].copy_from_slice(&stamp.fsid);
    put_u64(node, HDR_BYTENR, address);
    put_u64(node, HDR_FLAGS, HEADER_FLAG_WRITTEN | HEADER_BACKREF_REV_MIXED);
    node[HDR_CHUNK_TREE_UUID..HDR_CHUNK_TREE_UUID + 16].copy_from_slice(&stamp.chunk_uuid);
    put_u64(node, HDR_GENERATION, stamp.generation);
    put_u64(node, HDR_OWNER, owner);
    put_u32(node, HDR_NRITEMS, count as u32);
    node[HDR_LEVEL] = level;
}
