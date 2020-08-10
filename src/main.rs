use dkregistry::v2::Client as DKClient;
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::{
    api::{DeleteParams, ListParams, Meta},
    Api, Client,
};
use serde::Deserialize;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), ()> {
    let client = Client::try_default().await.unwrap();
    let namespaces = std::env::var("NAMESPACES")
        .unwrap_or("default".into())
        .replace(" ", "");
    loop {
        for namespace in namespaces.split(",") {
            let pod_api = Api::<Pod>::namespaced(client.clone(), &namespace);
            let secret_api = Api::<Secret>::namespaced(client.clone(), &namespace);
            let pods = match pod_api.list(&ListParams::default()).await {
                Ok(pods) => pods,
                Err(e) => {
                    println!("error {:?}", e);
                    continue;
                }
            };
            for p in pods.iter() {
                match &p.status {
                    Some(ps) => match &ps.container_statuses {
                        Some(vcs) => {
                            let mut csi = vcs.iter();
                            if loop {
                                if let Some(cs) = csi.next() {
                                    let s = cs.image.split("/").collect::<Vec<&str>>();
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
                                        println!(
                                            "cs image id:{:?} - new id {:?}",
                                            cs.image_id, new_id
                                        );
                                        if cs.image_id.split("@").collect::<Vec<&str>>()[1] != new_id {
                                            break true;
                                        }
                                    }
                                } else {
                                    break false;
                                }
                            } {
                                if let Err(e) =
                                    pod_api.delete(&p.name(), &DeleteParams::default()).await
                                {
                                    println!("{:?}", e)
                                }
                            }
                        }
                        None => {}
                    },
                    None => {}
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

async fn look_up_id(
    p: &Pod,
    registry: String,
    image: String,
    secret_api: &Api<Secret>,
) -> Option<String> {
    println!("check for image {:?} on registry {:?}", image, registry);
    if let Some(spec) = &p.spec {
        if let Some(vips) = &spec.image_pull_secrets {
            let secret = {
                let mut secret = None;
                for ips in vips {
                    if let Some(ips_name) = &ips.name {
                        if let Ok(pull_secret) = secret_api.get(&ips_name).await {
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
                let creds = String::from_utf8(base64::decode(creds).unwrap()).unwrap();
                let (username, password) = {
                    let s = creds.split(":").collect::<Vec<&str>>();
                    (s[0], s[1])
                };
                dclient = dclient
                    .username(Some(username.to_string()))
                    .password(Some(password.to_string()));
            }
            if let Ok(client) = dclient.build() {
                let client = if secret.is_some() {
                    if let Ok(client) = client.clone().authenticate(&vec!["registry"]).await {
                        client
                    } else {
                        client
                    }
                } else {
                    client
                };
                let (name, tag) = {
                    let s = image.split(":").collect::<Vec<&str>>();
                    (s[0], s[1])
                };
                match client.get_manifest_and_ref(name, tag).await {
                    Ok((_, hash)) => {
                        if let Some(hash) = hash {
                            return Some(hash);
                        }
                    }
                    Err(e) => println!("{:?}", e),
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
