use std::collections::HashMap;

#[macro_use]
extern crate log;
use simplelog::*;

use dkregistry::v2::Client as DKClient;
use k8s_openapi::api::core::v1::{Namespace, Pod, Secret};
use kube::{
    api::{DeleteParams, ListParams},
    Api, Client, ResourceExt,
};
use serde::Deserialize;

#[tokio::main]
async fn main() -> Result<(), ()> {
    let args: Vec<_> = std::env::args().collect();
    let level = if args.contains(&String::from("--debug")) {
        LevelFilter::Debug
    } else if args.contains(&String::from("--trace")) {
        LevelFilter::Trace
    } else {
        LevelFilter::Info
    };

    if let Err(e) = TermLogger::init(
        level,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    ) {
        println!("unable to initaliaze logger {:?}", e);
        std::process::exit(1)
    };

    let client = match Client::try_default().await {
        Ok(client) => client,
        Err(e) => {
            error!("unable to initalize kube client {:?}", e);
            std::process::exit(1)
        }
    };

    let namespaces = std::env::var("SHIPWRIGHT_NAMESPACES")
        .unwrap_or_else(|_| "default".into())
        .replace(" ", "");
    debug!("loading namespaces {:?}", namespaces);

    let interval = std::env::var("SHIPWRIGHT_INTERVAL")
        .unwrap_or_else(|_| "60".into())
        .parse::<u64>()
        .unwrap();
    debug!("setting check interval for {:?}", interval);

    info!("starting shipwright");
    loop {
        for namespace in namespaces.split(',') {
            if Api::<Namespace>::all(client.clone())
                .get(namespace)
                .await
                .is_ok()
            {
                check(namespace, &client).await;
            } else {
                warn!("unable to find namespace {:?} in cluster", namespace);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

async fn check(namespace: &str, client: &Client) {
    debug!("checking namespace {:?}", namespace);
    let pod_api = Api::<Pod>::namespaced(client.clone(), namespace);
    let secret_api = Api::<Secret>::namespaced(client.clone(), namespace);
    let pods = match pod_api.list(&ListParams::default()).await {
        Ok(pods) => {
            debug!("found {:?} pods", pods.items.len());
            pods
        }
        Err(e) => {
            error!("error {:?}", e);
            return;
        }
    };
    for p in pods.iter() {
        debug!("iterating over pod {:?}", p.name());
        if let Some(ps) = &p.status {
            let containers = match &p.spec {
                Some(spec) => spec.containers.clone(),
                None => Vec::new(),
            };
            if let Some(vcs) = &ps.container_statuses {
                let mut csi = vcs.iter();
                if loop {
                    if let Some(cs) = csi.next() {
                        let can_update_any = containers.iter().any(|c| {
                            c.image == Some(cs.image.clone())
                                && c.image_pull_policy == Some("Always".to_string())
                        });
                        if can_update_any {
                            let s = cs.image.split('/').collect::<Vec<&str>>();
                            if let Some(new_id) = look_up_id(
                                p,
                                {
                                    String::from(if s.len() > 2 {
                                        s[0]
                                    } else {
                                        "https://index.docker.io/v1"
                                    })
                                },
                                if s.len() > 2 {
                                    s[1..].join("/")
                                } else {
                                    cs.image.clone()
                                },
                                &secret_api,
                            )
                            .await
                            {
                                debug!("cs image id:{:?} - new id {:?}", cs.image_id, new_id);
                                if cs.image_id.contains('@') {
                                    if cs.image_id.split('@').collect::<Vec<&str>>()[1] != new_id {
                                        break true;
                                    }
                                } else {
                                    warn!(
                                        "unable to read image id {:?} for pod {:?}",
                                        cs.image_id,
                                        p.name()
                                    );
                                    break false;
                                }
                            }
                        }
                    } else {
                        break false;
                    }
                } {
                    debug!("attempting to delete pod {:?}", p.name());
                    match pod_api.delete(&p.name(), &DeleteParams::default()).await {
                        Ok(pod) => {
                            if let Some(po) = pod.left() {
                                while Api::<Pod>::namespaced(client.clone(), namespace)
                                    .get(&po.name())
                                    .await
                                    .is_ok()
                                {
                                    std::thread::sleep(std::time::Duration::from_millis(250));
                                }
                            }
                        }
                        Err(e) => error!("{:?}", e),
                    }
                    info!("pod {:?} refreshed", p.name());
                }
            }
        }
    }
}

async fn look_up_id(
    p: &Pod,
    registry: String,
    image: String,
    secret_api: &Api<Secret>,
) -> Option<String> {
    debug!("check for image {:?} on registry {:?}", image, registry);
    if let Some(spec) = &p.spec {
        if let Some(vips) = &spec.image_pull_secrets {
            let secret = {
                let mut secret = None;
                for ips in vips {
                    if let Some(ips_name) = &ips.name {
                        if let Ok(pull_secret) = secret_api.get(ips_name).await {
                            if let Some(data) = pull_secret.data {
                                if let Some(s) = data.get(".dockerconfigjson") {
                                    if let Ok(docker_auth) =
                                        serde_json::from_slice::<DockerConfigJson>(&s.0)
                                    {
                                        if let Some(docker_auth) = docker_auth.auths.get(&registry)
                                        {
                                            secret = Some(docker_auth.auth.to_string());
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                secret
            };
            let mut dclient = DKClient::configure()
                .insecure_registry(false)
                .registry(&registry);
            if let Some(creds) = &secret {
                let decoded_creds = match base64::decode(creds) {
                    Ok(c) => c,
                    Err(e) => {
                        error!("unable to decode creds {:?}", e);
                        return None;
                    }
                };

                let creds = match String::from_utf8(decoded_creds) {
                    Ok(c) => c,
                    Err(e) => {
                        error!("unable to parse creds to string {:?}", e);
                        return None;
                    }
                };

                let (username, password) = {
                    let s = creds.split(':').collect::<Vec<&str>>();
                    (s[0], s[1])
                };
                dclient = dclient
                    .username(Some(username.to_string()))
                    .password(Some(password.to_string()));
            }
            if let Ok(client) = dclient.build() {
                let client = if secret.is_some() {
                    if let Ok(client) = client.clone().authenticate(&["registry"]).await {
                        debug!("authenticated with registry");
                        client
                    } else {
                        client
                    }
                } else {
                    client
                };
                let (name, tag) = {
                    let s = image.split(':').collect::<Vec<&str>>();
                    (s[0], s[1])
                };
                match client.get_manifest_and_ref(name, tag).await {
                    Ok((_, hash)) => {
                        if let Some(hash) = hash {
                            debug!("found new image id {:?}", hash);
                            return Some(hash);
                        }
                    }
                    Err(e) => error!("{:?}", e),
                }
            }
        }
    }
    None
}

#[derive(Debug, Deserialize)]
pub struct DockerConfigJson {
    auths: HashMap<String, DockerConfigJsonAuth>,
}

#[derive(Debug, Deserialize)]
pub struct DockerConfigJsonAuth {
    auth: String,
}
