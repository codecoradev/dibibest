use dibibest_store::MemoryStore;

fn make() -> MemoryStore {
    MemoryStore::new()
}

dibibest_store::store_contract!(make);
