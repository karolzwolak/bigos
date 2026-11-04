const SPLIT_LINE_AT: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub struct LineMeta {
    pub first_pos: u64,
    pub one_past_last_pos: u64,
}

impl LineMeta {
    pub const fn empty(pos: u64) -> Self {
        Self {
            first_pos: pos,
            one_past_last_pos: pos,
        }
    }

    pub fn len(&self) -> usize {
        debug_assert!(self.one_past_last_pos >= self.first_pos);
        (self.one_past_last_pos - self.first_pos) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.first_pos == self.one_past_last_pos
    }
}

pub struct LineIndex {
    pub lines: alloc::vec::Vec<LineMeta>,
    pub current: usize,
    pub count: usize,
    pub max: usize,
}

impl LineIndex {
    pub fn new(max: usize) -> Self {
        let mut lines = alloc::vec::Vec::with_capacity(max);
        for _ in 0..max {
            lines.push(LineMeta::empty(0));
        }
        Self {
            lines,
            current: 0,
            count: 1,
            max,
        }
    }

    pub fn current_mut(&mut self) -> &mut LineMeta {
        &mut self.lines[self.current]
    }

    pub fn current_ref(&self) -> &LineMeta {
        &self.lines[self.current]
    }

    pub fn line_at(&self, idx: usize) -> &LineMeta {
        debug_assert!(idx < self.max);
        &self.lines[idx]
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn line_count(&self) -> usize {
        self.count
    }

    pub fn logical_line_for_index(&self, logical_idx: usize) -> Option<LineMeta> {
        if logical_idx >= self.count {
            return None;
        }
        let start = if self.count < self.max {
            0
        } else {
            (self.current + 1) % self.max
        };
        let phys = (start + logical_idx) % self.max;
        Some(self.lines[phys])
    }

    pub fn iter_logical(&self) -> impl Iterator<Item = LineMeta> + '_ {
        (0..self.count).filter_map(move |i| self.logical_line_for_index(i))
    }

    pub fn update_end(&mut self, at_p: u64) {
        self.lines[self.current].one_past_last_pos = at_p;
    }

    pub fn line_feed(&mut self, next_start: u64) {
        self.lines[self.current].one_past_last_pos = next_start;
        self.current += 1;
        if self.current >= self.max {
            self.current = 0;
        }

        self.lines[self.current] = LineMeta::empty(next_start);

        if self.count <= self.current {
            self.count = self.current + 1;
        }
        if self.count > self.max {
            self.count = self.max;
        }
    }

    pub fn force_line_feed_if_needed(&mut self, abs: u64) {
        if self.lines[self.current].len() >= SPLIT_LINE_AT {
            self.line_feed(abs);
        }
    }

    pub fn truncate_last(&mut self, new_end: u64) {
        let cur = self.lines[self.current].first_pos;
        debug_assert!(new_end >= cur);
        self.lines[self.current].one_past_last_pos = new_end;
    }

    pub fn clear(&mut self, pos: u64) {
        for l in &mut self.lines {
            *l = LineMeta::empty(pos);
        }

        self.current = 0;
        self.count = 1;
        self.lines[0] = LineMeta::empty(pos);
    }
}
