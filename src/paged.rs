use std::{
    ops::{Index, IndexMut},
    sync::Arc,
};

// Snapshots share pages; changing one record never copies the whole index.
#[derive(Clone)]
pub(crate) struct Paged<T, const N: usize = 4096> {
    pages: Vec<Arc<Vec<T>>>,
    len: usize,
}

impl<T: Clone, const N: usize> Paged<T, N> {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            pages: Vec::with_capacity(capacity.div_ceil(N)),
            len: 0,
        }
    }

    pub fn push(&mut self, value: T) {
        if self.len.is_multiple_of(N) {
            self.pages.push(Arc::new(Vec::with_capacity(N)));
        }
        Arc::make_mut(self.pages.last_mut().unwrap()).push(value);
        self.len += 1;
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn capacity(&self) -> usize {
        self.pages.iter().map(|p| p.capacity()).sum()
    }
    pub fn first(&self) -> Option<&T> {
        self.pages.first().and_then(|p| p.first())
    }
    pub fn chunks(&self) -> impl Iterator<Item = &[T]> {
        self.pages.iter().map(|p| p.as_slice())
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.pages.iter().flat_map(|p| p.iter())
    }
    pub fn pointers(&self) -> ReadPages<'_, T, N> {
        ReadPages {
            pointers: self.pages.iter().map(|p| p.as_ptr()).collect(),
            _owner: self,
        }
    }
}

impl<const N: usize> Paged<u8, N> {
    pub fn append(&mut self, value: &[u8]) -> usize {
        assert!(value.len() <= N);
        if self.len % N + value.len() > N {
            let page = Arc::make_mut(self.pages.last_mut().unwrap());
            self.len += N - page.len();
            page.resize(N, 0);
        }
        let start = self.len;
        if !value.is_empty() {
            if self.len.is_multiple_of(N) {
                self.pages.push(Arc::new(Vec::with_capacity(N)));
            }
            Arc::make_mut(self.pages.last_mut().unwrap()).extend_from_slice(value);
            self.len += value.len();
        }
        start
    }

    pub fn slice(&self, start: usize, len: usize) -> &[u8] {
        if len == 0 {
            return &[];
        }
        &self.pages[start / N][start % N..start % N + len]
    }
}

impl<T, const N: usize> Index<usize> for Paged<T, N> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.pages[i / N][i % N]
    }
}

impl<T: Clone, const N: usize> IndexMut<usize> for Paged<T, N> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut Arc::make_mut(&mut self.pages[i / N])[i % N]
    }
}

// The owner is immutably borrowed for the complete native call, including all workers.
pub(crate) struct ReadPages<'a, T, const N: usize> {
    pointers: Vec<*const T>,
    _owner: &'a Paged<T, N>,
}
unsafe impl<T: Sync, const N: usize> Sync for ReadPages<'_, T, N> {}
impl<T, const N: usize> ReadPages<'_, T, N> {
    pub fn as_ptr(&self) -> *const *const T {
        self.pointers.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_pages_and_cross_page_strings() {
        let mut original = Paged::<u8, 8>::with_capacity(16);
        assert_eq!(original.append(b"abcdef"), 0);
        let mut snapshot = original.clone();
        assert!(Arc::ptr_eq(&original.pages[0], &snapshot.pages[0]));
        assert_eq!(snapshot.append(b"xyz"), 8);
        snapshot[0] = b'X';
        assert_eq!(original.slice(0, 6), b"abcdef");
        assert_eq!(snapshot.slice(8, 3), b"xyz");
        assert_eq!(snapshot.slice(11, 0), b"");
        assert_eq!(snapshot.iter().count(), snapshot.len());
    }
}
