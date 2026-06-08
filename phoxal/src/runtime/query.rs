use std::num::NonZeroUsize;
use std::sync::Arc;

pub struct ReadCell<V> {
    inner: Arc<arc_swap::ArcSwap<V>>,
}

pub struct Reader<V> {
    inner: Arc<arc_swap::ArcSwap<V>>,
}

impl<V> Clone for Reader<V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<V: Send + Sync + 'static> ReadCell<V> {
    pub fn new(initial: V) -> Self {
        Self {
            inner: Arc::new(arc_swap::ArcSwap::from_pointee(initial)),
        }
    }

    pub fn publish(&self, next: V) {
        self.publish_arc(Arc::new(next));
    }

    pub fn publish_arc(&self, next: Arc<V>) {
        self.inner.store(next);
    }

    pub fn reader(&self) -> Reader<V> {
        Reader {
            inner: self.inner.clone(),
        }
    }

    pub fn load(&self) -> Arc<V> {
        self.inner.load_full()
    }
}

impl<V: Send + Sync + 'static> Reader<V> {
    pub fn load(&self) -> Arc<V> {
        self.inner.load_full()
    }
}

pub struct QueryOptions {
    pub max_in_flight: NonZeroUsize,
}

impl QueryOptions {
    pub fn single() -> Self {
        Self {
            max_in_flight: NonZeroUsize::new(1).expect("1 is non-zero"),
        }
    }

    pub fn max_in_flight(max: NonZeroUsize) -> Self {
        Self { max_in_flight: max }
    }
}

#[cfg(test)]
mod tests {
    use super::ReadCell;

    #[test]
    fn read_cell_loads_published_value() {
        let cell = ReadCell::new(1);

        cell.publish(2);

        assert_eq!(*cell.load(), 2);
        assert_eq!(*cell.reader().load(), 2);
    }

    #[test]
    fn read_cell_loaded_arc_pins_previous_snapshot() {
        let cell = ReadCell::new(String::from("old"));
        let old = cell.load();

        cell.publish(String::from("new"));

        assert_eq!(old.as_str(), "old");
        assert_eq!(cell.load().as_str(), "new");
    }
}
