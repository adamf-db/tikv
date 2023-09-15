// Copyright 2021 TiKV Project Authors. Licensed under Apache-2.0.
use std::path::Path;
use std::collections::HashMap;
use std::sync::Arc;
use std::fs::create_dir_all;

#[cfg(feature = "cloud-aws")]
use aws::{AwsKms, STORAGE_VENDOR_NAME_AWS};
#[cfg(feature = "cloud-azure")]
use azure::{AzureKms, STORAGE_VENDOR_NAME_AZURE};
use cloud::kms::Config as CloudConfig;
#[cfg(feature = "cloud-aws")]
pub use encryption::KmsBackend;
pub use encryption::{
    clean_up_dir, clean_up_trash, from_engine_encryption_method, trash_dir_all, AzureConfig,
    Backend, DataKeyImporter, DataKeyManager, DKMMap, DataKeyManagerArgs, DecrypterReader,
    EncryptionConfig, Error, FileConfig, Iv, KmsConfig, MasterKeyConfig, Result,
};
use encryption::{cloud_convert_error, FileBackend, PlaintextBackend};
use tikv_util::{box_err, error, info};

pub fn data_key_manager_from_config(
    config: &EncryptionConfig,
    dict_path: &str,
) -> Result<Option<DataKeyManager>> {
    info!("SINGLE DKM: Loading data key manager from config...");
    let master_key = create_backend(&config.master_key).map_err(|e| {
        error!("failed to access master key, {}", e);
        e
    })?;
    let args = DataKeyManagerArgs::from_encryption_config(dict_path, config);
    let previous_master_key_conf = config.previous_master_key.clone();
    let previous_master_key = Box::new(move || create_backend(&previous_master_key_conf));
    DataKeyManager::new(master_key, previous_master_key, 0, args)
}

pub fn data_key_manager_map_from_config(
    config: &EncryptionConfig,
    dict_path: &str,
) -> Result<DKMMap> {
    info!("MAP VERSION OF DKM LOADER: Loading data key manager from config...");
    let master_key = create_backend(&config.master_key).map_err(|e| {
        error!("failed to access master key, {}", e);
        e
    })?;

    let default_keyspace: u32 = 0;
    let file_dict_path = format!("{}/{}", dict_path, default_keyspace);

    info!("creating new dir if needed {}", file_dict_path);
    create_dir_all(file_dict_path.clone())?;
    let args = DataKeyManagerArgs::from_encryption_config(&file_dict_path, config);
    let previous_master_key_conf = config.previous_master_key.clone();
    let previous_master_key = Box::new(move || create_backend(&previous_master_key_conf));
    let mut dkm_map = HashMap::new();


    // master_key will have a keyspace_id of 0.
    let data_key_manager = DataKeyManager::new(master_key, previous_master_key, 0, args.clone())
        .unwrap().unwrap();

    dkm_map.insert(0, Arc::new(data_key_manager));
    for keyspace_config in &config.keyspace_keys {
        let keyspace_key = create_backend(&keyspace_config.key_config).map_err(|e| {
            error!("failed to access master key, {}", e);
            e
        })?;
        let previous_key_conf = keyspace_config.previous_key_config.clone();
        let previous_key = Box::new(move || create_backend(&previous_key_conf));
        let new_file_dict_path = format!("{}/{}", dict_path, keyspace_config.keyspace_id);
        info!("creating new dir if needed {}", new_file_dict_path);
        create_dir_all(new_file_dict_path.clone())?;
        let new_args = DataKeyManagerArgs::from_encryption_config(&new_file_dict_path, config);
        let key_manager = DataKeyManager::new(
            keyspace_key, previous_key,
            keyspace_config.keyspace_id, new_args.clone()).unwrap().unwrap();
        dkm_map.insert(keyspace_config.keyspace_id, Arc::new(key_manager));
    }

    info!("dkm_map len"; "dkm_map_len" => dkm_map.len());
    let dkmm = DKMMap::new(dkm_map);
    Ok(dkmm)
}


pub fn create_backend(config: &MasterKeyConfig) -> Result<Box<dyn Backend>> {
    let result = create_backend_inner(config);
    if let Err(e) = result {
        error!("failed to access master key, {}", e);
        return Err(e);
    };
    result
}

pub fn create_cloud_backend(config: &KmsConfig) -> Result<Box<dyn Backend>> {
    info!("Encryption init aws backend";
        "region" => &config.region,
        "endpoint" => &config.endpoint,
        "key_id" => &config.key_id,
        "vendor" => &config.vendor,
    );
    match config.vendor.as_str() {
        #[cfg(feature = "cloud-aws")]
        STORAGE_VENDOR_NAME_AWS | "" => {
            let conf = CloudConfig::from_proto(config.clone().into_proto())
                .map_err(cloud_convert_error("aws from proto".to_owned()))?;
            let kms_provider =
                Box::new(AwsKms::new(conf).map_err(cloud_convert_error("new AWS KMS".to_owned()))?);
            Ok(Box::new(KmsBackend::new(kms_provider)?) as Box<dyn Backend>)
        }
        #[cfg(feature = "cloud-azure")]
        STORAGE_VENDOR_NAME_AZURE => {
            if config.azure.is_none() {
                return Err(Error::Other(box_err!(
                    "invalid configurations for Azure KMS"
                )));
            }
            let (mk, azure_kms_cfg) = config.clone().convert_to_azure_kms_config();
            let conf = CloudConfig::from_azure_kms_config(mk, azure_kms_cfg)
                .map_err(cloud_convert_error("azure from proto".to_owned()))?;
            let keyvault_provider = Box::new(
                AzureKms::new(conf).map_err(cloud_convert_error("new Azure KMS".to_owned()))?,
            );
            Ok(Box::new(KmsBackend::new(keyvault_provider)?) as Box<dyn Backend>)
        }
        provider => Err(Error::Other(box_err!("provider not found {}", provider))),
    }
}

fn create_backend_inner(config: &MasterKeyConfig) -> Result<Box<dyn Backend>> {
    Ok(match config {
        MasterKeyConfig::Plaintext => Box::new(PlaintextBackend {}) as _,
        MasterKeyConfig::File { config } => {
            Box::new(FileBackend::new(Path::new(&config.path))?) as _
        }
        MasterKeyConfig::Kms { config } => return create_cloud_backend(config),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "cloud-azure")]
    fn test_kms_cloud_backend_azure() {
        let config = KmsConfig {
            key_id: "key_id".to_owned(),
            region: "region".to_owned(),
            endpoint: "endpoint".to_owned(),
            vendor: STORAGE_VENDOR_NAME_AZURE.to_owned(),
            azure: Some(AzureConfig {
                tenant_id: "tenant_id".to_owned(),
                client_id: "client_id".to_owned(),
                keyvault_url: "https://keyvault_url.vault.azure.net".to_owned(),
                hsm_name: "hsm_name".to_owned(),
                hsm_url: "https://hsm_url.managedhsm.azure.net/".to_owned(),
                client_secret: Some("client_secret".to_owned()),
                ..AzureConfig::default()
            }),
        };
        let invalid_config = KmsConfig {
            azure: None,
            ..config.clone()
        };
        create_cloud_backend(&invalid_config).unwrap_err();
        let backend = create_cloud_backend(&config).unwrap();
        assert!(backend.is_secure());
    }
}
