use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct MemoryPaths {
    pub base: PathBuf,
    pub memory_md: PathBuf,
    pub daily_dir: PathBuf,
    pub index_dir: PathBuf,
    pub sqlite_path: PathBuf,
}

impl MemoryPaths {
    pub fn new(base: PathBuf) -> Self {
        let memory_md = base.join("MEMORY.md");
        let daily_dir = base.join("daily");
        let index_dir = base.join("index");
        let sqlite_path = index_dir.join("memory.sqlite");
        Self {
            base,
            memory_md,
            daily_dir,
            index_dir,
            sqlite_path,
        }
    }

    pub fn daily_file(&self, ymd: &str) -> PathBuf {
        self.daily_dir.join(format!("{ymd}.md"))
    }
}
