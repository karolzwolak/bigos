/// Per operation logs of the containers, off because every serial byte costs about 87 us
pub const DATA_STRUCTURE_DEBUG_PRINT: bool = false;

pub mod circular_buffer;
pub mod dequeue;
pub mod hash_map_fx;
pub mod vector;
