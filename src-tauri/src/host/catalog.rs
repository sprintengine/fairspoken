//! Display metadata for every speech model the host can serve, published as
//! the `models` array of `/v1/stats` so dashboards can show installed vs
//! missing models without hard-coding the registry. The model registry itself
//! (ids, files, downloads) stays in `crate::models`.

use crate::models::{ModelService, SttModel};
use serde::Serialize;
use std::fs;
use std::path::Path;

struct CatalogEntry {
    id: &'static str,
    name: &'static str,
    publisher: &'static str,
    /// Download size, reported until the model is installed and its real
    /// on-disk size is known.
    approx_bytes: u64,
}

/// In the order the dashboard lists them: Parakeet family first (the default
/// engine), then Whisper by size. Ids that don't resolve in this build (Whisper
/// without the `whisper` feature) are skipped.
const CATALOG: [CatalogEntry; 10] = [
    CatalogEntry {
        id: "parakeet-tdt-0.6b-v3",
        name: "Parakeet TDT 0.6B v3",
        publisher: "NVIDIA",
        approx_bytes: 2_549_805_858,
    },
    CatalogEntry {
        id: "parakeet-ultra",
        name: "Parakeet Ultra 0.6B",
        publisher: "Moondream",
        approx_bytes: 2_595_892_056,
    },
    CatalogEntry {
        id: "parakeet-tdt-0.6b-v2",
        name: "Parakeet TDT 0.6B v2 (English)",
        publisher: "NVIDIA",
        approx_bytes: 2_549_805_858,
    },
    CatalogEntry {
        id: "tiny",
        name: "Whisper Tiny",
        publisher: "OpenAI",
        approx_bytes: 77_691_713,
    },
    CatalogEntry {
        id: "base",
        name: "Whisper Base",
        publisher: "OpenAI",
        approx_bytes: 147_951_465,
    },
    CatalogEntry {
        id: "small",
        name: "Whisper Small",
        publisher: "OpenAI",
        approx_bytes: 487_601_967,
    },
    CatalogEntry {
        id: "medium",
        name: "Whisper Medium",
        publisher: "OpenAI",
        approx_bytes: 1_533_763_059,
    },
    CatalogEntry {
        id: "large-v2",
        name: "Whisper Large v2",
        publisher: "OpenAI",
        approx_bytes: 3_094_623_691,
    },
    CatalogEntry {
        id: "large-v3",
        name: "Whisper Large v3",
        publisher: "OpenAI",
        approx_bytes: 3_095_033_483,
    },
    CatalogEntry {
        id: "large-v3-turbo",
        name: "Whisper Large v3 Turbo",
        publisher: "OpenAI",
        approx_bytes: 1_624_555_275,
    },
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelSnapshot {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
    pub(super) publisher: &'static str,
    pub(super) size_bytes: u64,
    pub(super) installed: bool,
    /// Indices of the workers configured to serve this model.
    pub(super) assigned_workers: Vec<usize>,
}

/// The disk-backed parts of a stats snapshot, read before the metrics lock
/// is taken: checking every model's files must never hold up the workers
/// and request handlers that wait on that lock.
pub(super) struct ModelInventory {
    /// Each worker's assigned model and whether its files are present.
    pub(super) workers: Vec<(SttModel, bool)>,
    pub(super) models: Vec<ModelSnapshot>,
}

impl ModelInventory {
    pub(super) fn read(models: &ModelService, worker_models: Vec<SttModel>) -> Self {
        let snapshots = model_snapshots(models, &worker_models);
        let workers = worker_models
            .into_iter()
            .map(|model| {
                // A cheap existence check, shared with the catalog row;
                // load/checksum failures surface through the worker state.
                let installed = snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == model.model_id())
                    .map(|snapshot| snapshot.installed)
                    .unwrap_or_else(|| models.files_present(model));
                (model, installed)
            })
            .collect();
        Self {
            workers,
            models: snapshots,
        }
    }
}

pub(super) fn model_snapshots(
    models: &ModelService,
    worker_models: &[SttModel],
) -> Vec<ModelSnapshot> {
    CATALOG
        .iter()
        .filter_map(|entry| {
            let model = SttModel::from_model_id(entry.id)?;
            let installed = models.files_present(model);
            let size_bytes = installed
                .then(|| installed_bytes(&models.path_for(model)))
                .filter(|bytes| *bytes > 0)
                .unwrap_or(entry.approx_bytes);
            Some(ModelSnapshot {
                id: entry.id,
                name: entry.name,
                publisher: entry.publisher,
                size_bytes,
                installed,
                assigned_workers: worker_models
                    .iter()
                    .enumerate()
                    .filter(|(_, assigned)| **assigned == model)
                    .map(|(index, _)| index)
                    .collect(),
            })
        })
        .collect()
}

/// A model is a single file (Whisper) or a directory of files (Parakeet).
fn installed_bytes(path: &Path) -> u64 {
    if path.is_file() {
        return fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.metadata().ok())
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{model_snapshots, ModelInventory, CATALOG};
    use crate::models::{ModelService, SttModel};

    #[test]
    fn catalog_covers_every_servable_model_with_assignments() {
        let models = ModelService::default();
        let snapshots = model_snapshots(&models, &[SttModel::Parakeet, SttModel::Parakeet]);

        let expected = CATALOG
            .iter()
            .filter(|entry| SttModel::from_model_id(entry.id).is_some())
            .count();
        assert_eq!(snapshots.len(), expected);
        assert!(snapshots
            .iter()
            .all(
                |model| SttModel::from_model_id(model.id).map(SttModel::model_id) == Some(model.id)
            ));
        let parakeet = &snapshots[0];
        assert_eq!(parakeet.id, "parakeet-tdt-0.6b-v3");
        assert_eq!(parakeet.publisher, "NVIDIA");
        assert_eq!(parakeet.assigned_workers, vec![0, 1]);
        assert!(snapshots[1..]
            .iter()
            .all(|model| model.assigned_workers.is_empty()));
        assert!(snapshots.iter().all(|model| model.size_bytes > 0));
    }

    #[test]
    fn inventory_reports_each_worker_with_its_catalog_install_state() {
        let models = ModelService::default();
        let inventory =
            ModelInventory::read(&models, vec![SttModel::Parakeet, SttModel::ParakeetUltra]);

        assert_eq!(inventory.workers.len(), 2);
        for (model, installed) in &inventory.workers {
            let listed = inventory
                .models
                .iter()
                .find(|snapshot| snapshot.id == model.model_id())
                .expect("assigned model listed");
            assert_eq!(*installed, listed.installed);
            assert_eq!(*installed, models.files_present(*model));
        }
        assert_eq!(inventory.workers[1].0, SttModel::ParakeetUltra);
    }
}
