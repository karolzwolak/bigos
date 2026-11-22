use crate::SyscallError;

pub const MAX_PATH_BYTES: usize = 256;
const SEPARATOR: u8 = b'/';
const CURRENT_DIR_DOTS: &str = ".";
const PARENT_DIR_DOTS: &str = "..";

#[derive(Clone, Copy)]
pub struct ShellPath {
    pub bytes: [u8; MAX_PATH_BYTES],
    pub len: usize,
}

impl ShellPath {
    pub const ROOT_DIR_PATH: Self = {
        let mut bytes = [0u8; MAX_PATH_BYTES];
        bytes[0] = SEPARATOR;

        Self { bytes, len: 1 }
    };

    pub fn resolve_from_cwd(cwd: &ShellPath, input: &str) -> Result<Self, SyscallError> {
        debug_assert!(cwd.is_normalized());

        let mut path = if input.as_bytes().first() == Some(&SEPARATOR) {
            Self::ROOT_DIR_PATH
        } else {
            *cwd
        };

        for component in input.split(SEPARATOR as char) {
            match component {
                "" | CURRENT_DIR_DOTS => {}
                PARENT_DIR_DOTS => path.pop_component(),
                name => path.push_component(name)?,
            }
        }

        debug_assert!(path.is_normalized());
        Ok(path)
    }

    pub fn as_str(&self) -> &str {
        debug_assert!(self.len <= MAX_PATH_BYTES);

        // SAFETY: built only from the root and whole components of str values
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }

    pub const fn is_root(&self) -> bool {
        self.len == 1
    }

    pub fn file_name(&self) -> &str {
        let path = self.as_str();
        match path.rfind(SEPARATOR as char) {
            Some(separator_index) => &path[separator_index + 1..],
            None => path,
        }
    }

    pub fn push_component(&mut self, name: &str) -> Result<(), SyscallError> {
        debug_assert!(!name.is_empty() && !name.contains(SEPARATOR as char));
        debug_assert!(name != CURRENT_DIR_DOTS && name != PARENT_DIR_DOTS);

        let separator_len = if self.is_root() { 0 } else { 1 };
        let new_len = self.len + separator_len + name.len();
        if new_len > MAX_PATH_BYTES {
            return Err(SyscallError::INVALID_PATH);
        }

        if separator_len != 0 {
            self.bytes[self.len] = SEPARATOR;
        }
        self.bytes[self.len + separator_len..new_len].copy_from_slice(name.as_bytes());
        self.len = new_len;

        Ok(())
    }

    pub fn child_absolute_path(&self, name: &str) -> Result<Self, SyscallError> {
        let mut joined = *self;
        joined.push_component(name)?;

        Ok(joined)
    }

    pub fn pop_component(&mut self) {
        if self.is_root() {
            return;
        }

        let separator_index = self.bytes[..self.len]
            .iter()
            .rposition(|&byte| byte == SEPARATOR)
            .unwrap_or(0);
        self.len = separator_index.max(1);
    }

    fn is_normalized(&self) -> bool {
        let path = &self.bytes[..self.len];
        let components_ok = path[1..].split(|&byte| byte == SEPARATOR).all(|component| {
            component != CURRENT_DIR_DOTS.as_bytes() && component != PARENT_DIR_DOTS.as_bytes()
        });

        path.first() == Some(&SEPARATOR)
            && (self.is_root() || path.last() != Some(&SEPARATOR))
            && !path.windows(2).any(|pair| pair == [SEPARATOR, SEPARATOR])
            && components_ok
    }
}

impl core::fmt::Display for ShellPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq for ShellPath {
    fn eq(&self, other: &Self) -> bool {
        self.as_str().eq_ignore_ascii_case(other.as_str())
    }
}
