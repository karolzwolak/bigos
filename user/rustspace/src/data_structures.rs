// from rustc-hash
// algorithm designed by Orson Peters
// only u64 support

extern crate alloc;

use alloc::vec::Vec;
use core::hash::Hasher;

const K: usize = 0xf1357aea2e62a9c5;

#[derive(Clone, Default)]
pub struct FxHasher {
    hash: usize,
}

impl FxHasher {
    #[inline(always)]
    fn add_to_hash(&mut self, i: usize) {
        self.hash = self.hash.wrapping_add(i).wrapping_mul(K);
    }

    #[inline(always)]
    fn finish_usize(&self) -> usize {
        const ROTATE: u32 = 26;
        self.hash.rotate_left(ROTATE)
    }
}

impl Hasher for FxHasher {
    #[inline(always)]
    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i);
    }

    #[inline(always)]
    fn write_u64(&mut self, i: u64) {
        self.add_to_hash(i as usize);
    }

    #[inline(always)]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.add_to_hash(b as usize);
        }
    }

    #[inline(always)]
    fn finish(&self) -> u64 {
        self.finish_usize() as u64
    }
}

const DEFAULT_CAPACITY: usize = 16;
const MAX_LOAD_FACTOR_NUM: usize = 3;
const MAX_LOAD_FACTOR_DEN: usize = 4; // 0.75

#[derive(Clone)]
enum Slot<K, V> {
    Empty,
    Dead,
    Occupied(K, V),
}

/// Designed for usize keys
pub struct FxHashMap<K, V> {
    slots: Vec<Slot<K, V>>,
    count: usize,
    // capacity is always slots.len(), always a power of two
}

impl<K, V> FxHashMap<K, V>
where
    K: Copy + Eq + Into<usize>,
{
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two().max(DEFAULT_CAPACITY);
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(Slot::Empty);
        }
        Self { slots, count: 0 }
    }

    pub fn default() -> Self {
        Self::new()
    }

    #[inline]
    fn get_slot_index(&self, key: K) -> usize {
        let mut hasher = FxHasher::default();
        hasher.add_to_hash(key.into());
        hasher.finish_usize() & (self.slots.len() - 1)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if (self.count + 1) * MAX_LOAD_FACTOR_DEN > self.slots.len() * MAX_LOAD_FACTOR_NUM {
            self.grow();
        }

        let capacity = self.slots.len();
        let start = self.get_slot_index(key);
        let mut first_dead: Option<usize> = None;

        for probe in 0..capacity {
            let idx = (start + probe) & (capacity - 1);
            match &self.slots[idx] {
                Slot::Empty => {
                    let insert_at = first_dead.unwrap_or(idx);
                    self.slots[insert_at] = Slot::Occupied(key, value);
                    self.count += 1;
                    return None;
                }
                Slot::Dead => {
                    if first_dead.is_none() {
                        first_dead = Some(idx);
                    }
                }
                Slot::Occupied(k, _) if *k == key => {
                    if let Slot::Occupied(_, old_val) =
                        core::mem::replace(&mut self.slots[idx], Slot::Occupied(key, value))
                    {
                        return Some(old_val);
                    }
                    unreachable!()
                }
                Slot::Occupied(..) => {}
            }
        }

        debug_assert!(false, "FxHashMap: no slot found after full probe");
        None
    }

    pub fn get(&self, key: K) -> Option<&V> {
        let capacity = self.slots.len();
        let start = self.get_slot_index(key);
        for probe in 0..capacity {
            let idx = (start + probe) & (capacity - 1);
            match &self.slots[idx] {
                Slot::Empty => return None,
                Slot::Dead => {}
                Slot::Occupied(k, v) if *k == key => return Some(v),
                Slot::Occupied(..) => {}
            }
        }
        None
    }

    pub fn get_mut(&mut self, key: K) -> Option<&mut V> {
        let cap = self.slots.len();
        let start = self.get_slot_index(key);
        let found_idx = 'search: {
            for probe in 0..cap {
                let idx = (start + probe) & (cap - 1);
                match &self.slots[idx] {
                    Slot::Empty => break 'search None,
                    Slot::Dead => {}
                    Slot::Occupied(k, _) if *k == key => break 'search Some(idx),
                    Slot::Occupied(..) => {}
                }
            }
            None
        };
        match found_idx {
            Some(idx) => {
                if let Slot::Occupied(_, v) = &mut self.slots[idx] {
                    Some(v)
                } else {
                    unreachable!()
                }
            }
            None => None,
        }
    }

    pub fn contains_key(&self, key: K) -> bool {
        self.get(key).is_some()
    }

    pub fn remove(&mut self, key: K) -> Option<V> {
        let cap = self.slots.len();
        let start = self.get_slot_index(key);
        for probe in 0..cap {
            let idx = (start + probe) & (cap - 1);
            match &self.slots[idx] {
                Slot::Empty => return None,
                Slot::Dead => {}
                Slot::Occupied(k, _) if *k == key => {
                    if let Slot::Occupied(_, v) =
                        core::mem::replace(&mut self.slots[idx], Slot::Dead)
                    {
                        self.count -= 1;
                        return Some(v);
                    }
                    unreachable!()
                }
                Slot::Occupied(..) => {}
            }
        }
        None
    }

    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        if (self.count + 1) * MAX_LOAD_FACTOR_DEN > self.slots.len() * MAX_LOAD_FACTOR_NUM {
            self.grow();
        }

        let capacity = self.slots.len();
        let start = self.get_slot_index(key);
        let mut first_dead: Option<usize> = None;

        for probe in 0..capacity {
            let idx = (start + probe) & (capacity - 1);
            match &self.slots[idx] {
                Slot::Empty => {
                    let insert_at = first_dead.unwrap_or(idx);
                    return Entry::Vacant(VacantEntry {
                        map: self,
                        key,
                        idx: insert_at,
                    });
                }
                Slot::Dead => {
                    if first_dead.is_none() {
                        first_dead = Some(idx);
                    }
                }
                Slot::Occupied(k, _) if *k == key => {
                    return Entry::Occupied(OccupiedEntry { map: self, idx });
                }
                Slot::Occupied(..) => {}
            }
        }

        debug_assert!(false, "FxHashMap: entry() found no slot");
        Entry::Vacant(VacantEntry {
            map: self,
            key,
            idx: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = Slot::Empty;
        }
        self.count = 0;
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.slots.iter().filter_map(|slot| {
            if let Slot::Occupied(k, v) = slot {
                Some((k, v))
            } else {
                None
            }
        })
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.slots.iter_mut().filter_map(|slot| {
            if let Slot::Occupied(k, v) = slot {
                Some((&*k, v))
            } else {
                None
            }
        })
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.slots.iter().filter_map(|slot| {
            if let Slot::Occupied(_, v) = slot {
                Some(v)
            } else {
                None
            }
        })
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.slots.iter_mut().filter_map(|slot| {
            if let Slot::Occupied(_, v) = slot {
                Some(v)
            } else {
                None
            }
        })
    }

    fn grow(&mut self) {
        let new_cap = (self.slots.len() * 2).max(DEFAULT_CAPACITY);
        let mut new_slots: Vec<Slot<K, V>> = Vec::with_capacity(new_cap);
        for _ in 0..new_cap {
            new_slots.push(Slot::Empty);
        }
        let old_slots = core::mem::replace(&mut self.slots, new_slots);
        self.count = 0;
        for slot in old_slots {
            if let Slot::Occupied(k, v) = slot {
                self.insert(k, v);
            }
        }
    }
}

unsafe impl<K: Send, V: Send> Send for FxHashMap<K, V> {}
unsafe impl<K: Sync, V: Sync> Sync for FxHashMap<K, V> {}

pub enum Entry<'a, K, V>
where
    K: Copy + Eq + Into<usize>,
{
    Occupied(OccupiedEntry<'a, K, V>),
    Vacant(VacantEntry<'a, K, V>),
}

impl<'a, K, V> Entry<'a, K, V>
where
    K: Copy + Eq + Into<usize>,
{
    pub fn or_insert_with<F: FnOnce() -> V>(self, f: F) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(f()),
        }
    }

    pub fn or_insert(self, default: V) -> &'a mut V {
        self.or_insert_with(|| default)
    }
}

pub struct OccupiedEntry<'a, K, V>
where
    K: Copy + Eq + Into<usize>,
{
    map: &'a mut FxHashMap<K, V>,
    idx: usize,
}

impl<'a, K, V> OccupiedEntry<'a, K, V>
where
    K: Copy + Eq + Into<usize>,
{
    pub fn into_mut(self) -> &'a mut V {
        if let Slot::Occupied(_, v) = &mut self.map.slots[self.idx] {
            return v;
        }
        unreachable!()
    }
}

pub struct VacantEntry<'a, K, V>
where
    K: Copy + Eq + Into<usize>,
{
    map: &'a mut FxHashMap<K, V>,
    key: K,
    idx: usize,
}

impl<'a, K, V> VacantEntry<'a, K, V>
where
    K: Copy + Eq + Into<usize>,
{
    pub fn insert(self, value: V) -> &'a mut V {
        self.map.slots[self.idx] = Slot::Occupied(self.key, value);
        self.map.count += 1;
        if let Slot::Occupied(_, v) = &mut self.map.slots[self.idx] {
            return v;
        }
        unreachable!()
    }
}
