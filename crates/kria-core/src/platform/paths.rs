use std::path::PathBuf;

/// All standard data paths for KRIA.
#[derive(Debug, Clone)]
pub struct KriaPaths {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub models_dir: PathBuf,
    pub llm_models: PathBuf,
    pub stt_models: PathBuf,
    pub tts_models: PathBuf,
    pub embedding_models: PathBuf,
    pub db_path: PathBuf,
    pub vectors_path: PathBuf,
    pub rollback_dir: PathBuf,
    pub workflows_dir: PathBuf,
    pub plugins_dir: PathBuf,
    pub logs_dir: PathBuf,
}

impl KriaPaths {
    /// Resolve all paths. Creates directories if they don't exist.
    pub fn resolve() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let config_dir = home.join(".kria");
        let data_dir = config_dir.clone();
        let models_dir = config_dir.join("models");
        let logs_dir = config_dir.join("logs");

        let paths = Self {
            home,
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            models_dir: models_dir.clone(),
            llm_models: models_dir.join("llm"),
            stt_models: models_dir.join("stt"),
            tts_models: models_dir.join("tts"),
            embedding_models: models_dir.join("embeddings"),
            db_path: data_dir.join("kria.db"),
            vectors_path: data_dir.join("vectors.usearch"),
            rollback_dir: data_dir.join("rollback"),
            workflows_dir: data_dir.join("workflows"),
            plugins_dir: data_dir.join("plugins"),
            logs_dir,
        };

        paths.ensure_dirs();
        paths
    }

    fn ensure_dirs(&self) {
        let dirs = [
            &self.config_dir,
            &self.models_dir,
            &self.llm_models,
            &self.stt_models,
            &self.tts_models,
            &self.embedding_models,
            &self.rollback_dir,
            &self.workflows_dir,
            &self.plugins_dir,
            &self.logs_dir,
        ];
        for d in dirs {
            let _ = std::fs::create_dir_all(d);
        }
    }

    /// Path for user config override file.
    pub fn user_config(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}
