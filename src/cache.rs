use parking_lot::Mutex;
use std::mem;

pub(crate) struct CacheMutex<T: Clone> {
    m: Mutex<Option<T>>,
}

impl<T: Clone> Clone for CacheMutex<T> {
    fn clone(&self) -> Self {
        let mut me = self.m.lock();
        let value = mem::replace(&mut *me, None);
        *me = value.clone();
        Self {
            m: Mutex::new(value),
        }
    }
}

impl<T: Clone> Default for CacheMutex<T> {
    fn default() -> Self {
        Self {
            m: Mutex::new(None),
        }
    }
}

impl<T: Clone> CacheMutex<T> {
    pub fn get<E>(&self, factory: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let mut me = self.m.lock();
        let value = match mem::replace(&mut *me, None) {
            Some(value) => value,
            None => factory()?,
        };
        *me = Some(value.clone());
        Ok(value)
    }

    pub fn clear(&self) {
        *self.m.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;

    #[test]
    fn test_cache_mutex() {
        let cache: CacheMutex<String> = Default::default();

        let value = cache
            .get(|| Ok::<_, Infallible>("Hello, World!".to_string()))
            .unwrap();
        assert_eq!(value, "Hello, World!");
        let value = cache
            .get(|| -> Result<String, Infallible> { panic!("I should not run") })
            .unwrap();
        assert_eq!(value, "Hello, World!");

        let new_cache = cache.clone(); // clone should not touch the cache
        let value = cache
            .get(|| -> Result<String, Infallible> { panic!("I should not run") })
            .unwrap();
        assert_eq!(value, "Hello, World!");

        cache.clear();
        let value = cache
            .get(|| Ok::<_, Infallible>("Hi, Amigo!".to_string()))
            .unwrap();
        assert_eq!(value, "Hi, Amigo!");
        let value = new_cache
            .get(|| -> Result<String, Infallible> { panic!("I should not run") })
            .unwrap();
        assert_eq!(value, "Hello, World!");
    }
}
