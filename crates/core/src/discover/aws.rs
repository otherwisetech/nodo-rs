//! AWS, read-only, through the user's own AWS CLI and profile (SSO included): VPCs, subnets,
//! EC2 instances, security groups and Client VPN endpoints, per region. Nodo never sees or
//! stores credentials; it runs `aws ec2 describe-* --output json` and reads the JSON.
use crate::model::{Discovery, IdentKind, Layer, NetworkObservation, Observation, Source};
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

pub const EXTRA_PATHS: &[&str] = &["/usr/local/bin/aws", "/opt/homebrew/bin/aws", "C:\\Program Files\\Amazon\\AWSCLIV2\\aws.exe"];

pub fn find_cli() -> Option<PathBuf> {
    crate::cmd::find("aws", EXTRA_PATHS)
}

fn tag_name(v: &Value) -> Option<String> {
    v.get("Tags")?.as_array()?.iter().find(|t| t.get("Key").and_then(Value::as_str) == Some("Name")).and_then(|t| t.get("Value")?.as_str()).map(str::to_string).filter(|s| !s.is_empty())
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.is_empty())
}

fn arr<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

pub fn vpc_key(profile: &str, region: &str, id: &str) -> String {
    format!("aws:{profile}:{region}:{id}")
}

/// `describe-vpcs`.
pub fn parse_vpcs(json: &str, profile: &str, region: &str) -> Result<Vec<NetworkObservation>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(arr(&v, "Vpcs")
        .filter_map(|x| {
            let id = s(x, "VpcId")?;
            let mut n = NetworkObservation { key: vpc_key(profile, region, &id), name: tag_name(x).unwrap_or_else(|| id.clone()), layer: Some(Layer::Aws), cidr: s(x, "CidrBlock"), ..Default::default() };
            n.facts = vec![("vpc_id".into(), id), ("region".into(), region.into()), ("profile".into(), profile.into())];
            if x.get("IsDefault").and_then(Value::as_bool) == Some(true) {
                n.facts.push(("default_vpc".into(), "1".into()));
            }
            Some(n)
        })
        .collect())
}

/// `describe-subnets`: each subnet is a network inside its VPC.
pub fn parse_subnets(json: &str, profile: &str, region: &str) -> Result<Vec<NetworkObservation>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(arr(&v, "Subnets")
        .filter_map(|x| {
            let id = s(x, "SubnetId")?;
            let mut n = NetworkObservation { key: vpc_key(profile, region, &id), name: tag_name(x).unwrap_or_else(|| id.clone()), layer: Some(Layer::Aws), cidr: s(x, "CidrBlock"), parent_key: s(x, "VpcId").map(|vpc| vpc_key(profile, region, &vpc)), ..Default::default() };
            n.facts = vec![("subnet_id".into(), id), ("az".into(), s(x, "AvailabilityZone").unwrap_or_default())];
            if x.get("MapPublicIpOnLaunch").and_then(Value::as_bool) == Some(true) {
                n.facts.push(("public".into(), "1".into()));
            }
            Some(n)
        })
        .collect())
}

/// `describe-security-groups`: id → name (with the inbound ports summarised).
pub fn parse_security_groups(json: &str) -> Result<BTreeMap<String, String>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(arr(&v, "SecurityGroups")
        .filter_map(|g| {
            let id = s(g, "GroupId")?;
            let name = s(g, "GroupName").unwrap_or_else(|| id.clone());
            let mut ports: Vec<String> = arr(g, "IpPermissions")
                .map(|p| match (p.get("FromPort").and_then(Value::as_i64), p.get("ToPort").and_then(Value::as_i64), s(p, "IpProtocol").as_deref()) {
                    (_, _, Some("-1")) => "all".to_string(), // i18n-ok: data value
                    (Some(a), Some(b), _) if a == b => a.to_string(),
                    (Some(a), Some(b), _) => format!("{a}-{b}"),
                    _ => "?".into(),
                })
                .collect();
            ports.sort();
            ports.dedup();
            Some((id, if ports.is_empty() { name } else { format!("{name} ({})", ports.join(", ")) }))
        })
        .collect())
}

/// `describe-instances`.
pub fn parse_instances(json: &str, profile: &str, region: &str, groups: &BTreeMap<String, String>) -> Result<Vec<Observation>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in arr(&v, "Reservations") {
        for i in arr(r, "Instances") {
            let Some(id) = s(i, "InstanceId") else { continue };
            let mut o = Observation::new(Layer::Aws, Source::Aws).ident(IdentKind::AwsInstance, &id);
            o.name = Some(tag_name(i).unwrap_or_else(|| id.clone()));
            o.kind = Some(crate::model::DeviceKind::Vm);
            let state = i.pointer("/State/Name").and_then(Value::as_str).unwrap_or_default().to_string();
            o.online = Some(state == "running");
            let mut ips: Vec<IpAddr> = Vec::new();
            if let Some(ip) = s(i, "PrivateIpAddress").and_then(|x| x.parse().ok()) {
                ips.push(ip);
            }
            for ni in arr(i, "NetworkInterfaces") {
                for pa in arr(ni, "PrivateIpAddresses") {
                    if let Some(ip) = s(pa, "PrivateIpAddress").and_then(|x| x.parse().ok())
                        && !ips.contains(&ip)
                    {
                        ips.push(ip);
                    }
                }
                for v6 in arr(ni, "Ipv6Addresses") {
                    if let Some(ip) = s(v6, "Ipv6Address").and_then(|x| x.parse().ok()) {
                        ips.push(ip);
                    }
                }
                if let Some(m) = s(ni, "MacAddress") {
                    o = o.ident(IdentKind::Mac, &m);
                }
            }
            o.ips = ips;
            o.hostname = s(i, "PrivateDnsName");
            o.os = s(i, "PlatformDetails").or_else(|| s(i, "Platform"));
            o.network = s(i, "SubnetId").map(|sn| vpc_key(profile, region, &sn)).or_else(|| s(i, "VpcId").map(|vpc| vpc_key(profile, region, &vpc)));
            let sgs: Vec<String> = arr(i, "SecurityGroups").filter_map(|g| s(g, "GroupId").map(|gid| groups.get(&gid).cloned().or_else(|| s(g, "GroupName")).unwrap_or(gid))).collect();
            o = o
                .fact("instance_id", id.clone())
                .fact("instance_type", s(i, "InstanceType").unwrap_or_default())
                .fact("state", state)
                .fact("az", i.pointer("/Placement/AvailabilityZone").and_then(Value::as_str).unwrap_or_default())
                .fact("public_ip", s(i, "PublicIpAddress").unwrap_or_default())
                .fact("key_name", s(i, "KeyName").unwrap_or_default())
                .fact("security_groups", sgs.join("; "))
                .fact("region", region)
                .fact("profile", profile);
            out.push(o);
        }
    }
    Ok(out)
}

/// `describe-client-vpn-endpoints`: each endpoint is a VPN node in its VPC.
pub fn parse_client_vpn(json: &str, profile: &str, region: &str) -> Result<Vec<Observation>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(arr(&v, "ClientVpnEndpoints")
        .filter_map(|e| {
            let id = s(e, "ClientVpnEndpointId")?;
            let mut o = Observation::new(Layer::Aws, Source::Aws).ident(IdentKind::AwsInstance, &id);
            o.name = Some(tag_name(e).or_else(|| s(e, "Description")).unwrap_or_else(|| id.clone()));
            o.kind = Some(crate::model::DeviceKind::Vpn);
            let status = e.pointer("/Status/Code").and_then(Value::as_str).unwrap_or_default().to_string();
            o.online = Some(status == "available");
            o.network = s(e, "VpcId").map(|vpc| vpc_key(profile, region, &vpc));
            o.hostname = s(e, "DnsName");
            Some(o.fact("client_cidr", s(e, "ClientCidrBlock").unwrap_or_default()).fact("state", status).fact("vpn_protocol", s(e, "VpnProtocol").unwrap_or_default()).fact("region", region).fact("profile", profile))
        })
        .collect())
}

/// A CLI failure in plain words: expired SSO session, no credentials, access denied.
pub fn explain_error(stderr: &str, profile: &str) -> String {
    let e = stderr.trim();
    let l = e.to_lowercase();
    if l.contains("sso") && (l.contains("expired") || l.contains("token")) || l.contains("error loading sso token") {
        i18n::t!("core.aws.sso_expired", profile = profile)
    } else if l.contains("unable to locate credentials") || l.contains("could not be found") && l.contains("profile") {
        i18n::t!("core.aws.no_credentials", profile = profile)
    } else if l.contains("unauthorizedoperation") || l.contains("accessdenied") || l.contains("not authorized") {
        i18n::t!("core.aws.denied", detail = e.lines().last().unwrap_or(e))
    } else {
        i18n::t!("core.aws.failed", detail = e.lines().last().unwrap_or(e))
    }
}

/// `aws configure list-profiles`.
pub fn list_profiles() -> Vec<String> {
    let Some(cli) = find_cli() else { return Vec::new() };
    crate::cmd::run(&cli, &["configure", "list-profiles"], Duration::from_secs(10)).map(|o| o.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

/// The profile's configured region, if any.
pub fn default_region(profile: &str) -> Option<String> {
    let cli = find_cli()?;
    crate::cmd::run(&cli, &["configure", "get", "region", "--profile", profile], Duration::from_secs(10)).ok().map(|o| o.stdout.trim().to_string()).filter(|r| !r.is_empty())
}

/// Everything for `profile` in each region. Read-only describe calls.
pub fn discover(profile: &str, regions: &[String]) -> Discovery {
    let mut d = Discovery::default();
    let Some(cli) = find_cli() else {
        d.notes.push((Source::Aws, i18n::t!("core.aws.not_found")));
        return d;
    };
    let t = Duration::from_secs(60);
    for region in regions {
        let call = |what: &str| -> Result<String, String> {
            match crate::cmd::run(&cli, &["ec2", what, "--profile", profile, "--region", region, "--output", "json"], t) {
                Ok(o) if o.ok => Ok(o.stdout),
                Ok(o) => Err(explain_error(&o.stderr, profile)),
                Err(e) => Err(i18n::t!("core.aws.failed", detail = format!("{e:?}"))),
            }
        };
        let vpcs = match call("describe-vpcs").and_then(|j| parse_vpcs(&j, profile, region)) {
            Ok(v) => v,
            Err(e) => {
                d.notes.push((Source::Aws, format!("{region}: {e}"))); // i18n-ok: region prefix
                continue;
            }
        };
        d.networks.extend(vpcs);
        if let Ok(sn) = call("describe-subnets").and_then(|j| parse_subnets(&j, profile, region)) {
            d.networks.extend(sn);
        }
        let groups = call("describe-security-groups").and_then(|j| parse_security_groups(&j)).unwrap_or_default();
        match call("describe-instances").and_then(|j| parse_instances(&j, profile, region, &groups)) {
            Ok(i) => d.devices.extend(i),
            Err(e) => d.notes.push((Source::Aws, format!("{region}: {e}"))), // i18n-ok: region prefix
        }
        // Client VPN needs its own permission; quietly skip when denied
        if let Ok(v) = call("describe-client-vpn-endpoints").and_then(|j| parse_client_vpn(&j, profile, region)) {
            d.devices.extend(v);
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const VPCS: &str = r#"{"Vpcs":[{"VpcId":"vpc-0a1","CidrBlock":"10.0.0.0/16","IsDefault":false,"Tags":[{"Key":"Name","Value":"prod"}]},{"VpcId":"vpc-0b2","CidrBlock":"172.31.0.0/16","IsDefault":true}]}"#;
    const SUBNETS: &str = r#"{"Subnets":[{"SubnetId":"subnet-1","VpcId":"vpc-0a1","CidrBlock":"10.0.1.0/24","AvailabilityZone":"us-east-1a","MapPublicIpOnLaunch":true,"Tags":[{"Key":"Name","Value":"public-a"}]},{"SubnetId":"subnet-2","VpcId":"vpc-0a1","CidrBlock":"10.0.2.0/24","AvailabilityZone":"us-east-1b"}]}"#;
    const SGS: &str = r#"{"SecurityGroups":[{"GroupId":"sg-1","GroupName":"web","IpPermissions":[{"IpProtocol":"tcp","FromPort":443,"ToPort":443},{"IpProtocol":"tcp","FromPort":22,"ToPort":22}]},{"GroupId":"sg-2","GroupName":"internal","IpPermissions":[{"IpProtocol":"-1"}]}]}"#;
    const INSTANCES: &str = r#"{"Reservations":[{"Instances":[
      {"InstanceId":"i-0abc","InstanceType":"t3.small","State":{"Code":16,"Name":"running"},"PrivateIpAddress":"10.0.1.10","PublicIpAddress":"54.1.2.3","PrivateDnsName":"ip-10-0-1-10.ec2.internal","SubnetId":"subnet-1","VpcId":"vpc-0a1","Placement":{"AvailabilityZone":"us-east-1a"},"PlatformDetails":"Linux/UNIX","KeyName":"juan","SecurityGroups":[{"GroupId":"sg-1","GroupName":"web"}],"NetworkInterfaces":[{"MacAddress":"0e:11:22:33:44:55","PrivateIpAddresses":[{"PrivateIpAddress":"10.0.1.10"},{"PrivateIpAddress":"10.0.1.11"}]}],"Tags":[{"Key":"Name","Value":"web-1"}]},
      {"InstanceId":"i-0def","InstanceType":"t3.micro","State":{"Code":80,"Name":"stopped"},"PrivateIpAddress":"10.0.2.20","SubnetId":"subnet-2","VpcId":"vpc-0a1","SecurityGroups":[{"GroupId":"sg-2","GroupName":"internal"}]}
    ]}]}"#;
    const CVPN: &str = r#"{"ClientVpnEndpoints":[{"ClientVpnEndpointId":"cvpn-endpoint-0123","Description":"Office VPN","Status":{"Code":"available"},"ClientCidrBlock":"10.100.0.0/22","DnsName":"*.cvpn-endpoint-0123.prod.clientvpn.us-east-1.amazonaws.com","VpcId":"vpc-0a1","VpnProtocol":"openvpn"}]}"#;

    #[test]
    fn aws_fixtures() {
        let v = parse_vpcs(VPCS, "dev", "us-east-1").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "prod");
        assert_eq!(v[0].key, "aws:dev:us-east-1:vpc-0a1");
        assert_eq!(v[1].name, "vpc-0b2");
        assert!(v[1].facts.contains(&("default_vpc".into(), "1".into())));
        let s = parse_subnets(SUBNETS, "dev", "us-east-1").unwrap();
        assert_eq!(s[0].parent_key.as_deref(), Some("aws:dev:us-east-1:vpc-0a1"));
        assert_eq!(s[0].cidr.as_deref(), Some("10.0.1.0/24"));
        let g = parse_security_groups(SGS).unwrap();
        assert_eq!(g["sg-1"], "web (22, 443)");
        assert_eq!(g["sg-2"], "internal (all)");
        let i = parse_instances(INSTANCES, "dev", "us-east-1", &g).unwrap();
        assert_eq!(i.len(), 2);
        assert_eq!(i[0].name.as_deref(), Some("web-1"));
        assert_eq!(i[0].ips.len(), 2);
        assert_eq!(i[0].online, Some(true));
        assert_eq!(i[0].network.as_deref(), Some("aws:dev:us-east-1:subnet-1"));
        assert!(i[0].facts.contains(&("security_groups".into(), "web (22, 443)".into())));
        assert!(i[0].facts.contains(&("public_ip".into(), "54.1.2.3".into())));
        assert!(i[0].idents.iter().any(|x| x.kind == IdentKind::Mac));
        assert_eq!(i[1].name.as_deref(), Some("i-0def"));
        assert_eq!(i[1].online, Some(false));
        let c = parse_client_vpn(CVPN, "dev", "us-east-1").unwrap();
        assert_eq!(c[0].name.as_deref(), Some("Office VPN"));
        assert_eq!(c[0].kind, Some(crate::model::DeviceKind::Vpn));
        assert!(c[0].facts.contains(&("client_cidr".into(), "10.100.0.0/22".into())));
        assert!(parse_vpcs("{", "p", "r").is_err());
        i18n::set_locale("en");
        assert!(explain_error("Error when retrieving token from sso: Token has expired and refresh failed", "dev").contains("aws sso login --profile dev"));
        assert!(explain_error("Unable to locate credentials. You can configure credentials by running \"aws configure\".", "dev").contains("dev"));
    }
}
