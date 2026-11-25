//! W20b P-G14: persistent raw WFP filters (built only with `--features wfp`, so a mistake here cannot break the rest of the
//! probe). Windows Defender Firewall rules never filter loopback, so a box's loopback is fenced here instead: our own provider
//! and sublayer, filters at ALE_AUTH_CONNECT v4/v6 that block everything the match names except loopback to one port.
//!
//! `wfp-install --sd SDDL | --sid PACKAGE_SID [--permit-sd SDDL] --port P` and `wfp-remove`. Nothing here ships.
use super::rbox::{sddl_to_sd, sid_from};
use super::*;
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows_sys::core::GUID;

const PROVIDER: GUID = GUID::from_u128(0x6b1c4e00_5a1d_4c0e_9a11_0f3c2b7d9e01);
const SUBLAYER: GUID = GUID::from_u128(0x6b1c4e00_5a1d_4c0e_9a11_0f3c2b7d9e02);

fn fkey(i: u32) -> GUID {
    GUID::from_u128(0x6b1c4e00_5a1d_4c0e_9a11_0f3c2b7d9f00 + u128::from(i))
}

enum Cond {
    Package(String),
    UserSd(String),
    Range(u32, u32),
    PortNot(u16),
}

unsafe fn u32_value(x: u32) -> FWP_VALUE0 {
    let mut v: FWP_VALUE0 = std::mem::zeroed();
    v.r#type = FWP_UINT32;
    v.Anonymous.uint32 = x;
    v
}

/// One filter, persistent, in our sublayer. `keep` holds the memory the conditions point at until the call returns.
unsafe fn add(h: HANDLE, name: &str, idx: u32, layer: GUID, weight: u8, action: u32, conds: &[Cond]) -> Res<()> {
    let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
    let mut cs: Vec<FWPM_FILTER_CONDITION0> = Vec::new();
    for c in conds {
        let mut fc: FWPM_FILTER_CONDITION0 = std::mem::zeroed();
        match c {
            Cond::Package(sid) => {
                fc.fieldKey = FWPM_CONDITION_ALE_PACKAGE_ID;
                fc.matchType = FWP_MATCH_EQUAL;
                fc.conditionValue.r#type = FWP_SID;
                fc.conditionValue.Anonymous.sid = sid_from(sid)?.cast();
            }
            Cond::UserSd(sddl) => {
                let sd = sddl_to_sd(sddl)?;
                let len = windows_sys::Win32::Security::GetSecurityDescriptorLength(sd);
                let mut blob = Box::new(FWP_BYTE_BLOB { size: len, data: sd.cast() });
                fc.fieldKey = FWPM_CONDITION_ALE_USER_ID;
                fc.matchType = FWP_MATCH_EQUAL;
                fc.conditionValue.r#type = FWP_SECURITY_DESCRIPTOR_TYPE;
                fc.conditionValue.Anonymous.sd = &mut *blob;
                keep.push(blob);
            }
            Cond::Range(lo, hi) => {
                let mut range = Box::new(FWP_RANGE0 { valueLow: u32_value(*lo), valueHigh: u32_value(*hi) });
                fc.fieldKey = FWPM_CONDITION_IP_REMOTE_ADDRESS;
                fc.matchType = FWP_MATCH_RANGE;
                fc.conditionValue.r#type = FWP_RANGE_TYPE;
                fc.conditionValue.Anonymous.rangeValue = &mut *range;
                keep.push(range);
            }
            Cond::PortNot(p) => {
                fc.fieldKey = FWPM_CONDITION_IP_REMOTE_PORT;
                fc.matchType = FWP_MATCH_NOT_EQUAL;
                fc.conditionValue.r#type = FWP_UINT16;
                fc.conditionValue.Anonymous.uint16 = *p;
            }
        }
        cs.push(fc);
    }
    let mut nm = wide(name);
    let mut provider = PROVIDER;
    let mut f: FWPM_FILTER0 = std::mem::zeroed();
    f.filterKey = fkey(idx);
    f.displayData.name = nm.as_mut_ptr();
    f.flags = FWPM_FILTER_FLAG_PERSISTENT;
    f.providerKey = &mut provider;
    f.layerKey = layer;
    f.subLayerKey = SUBLAYER;
    f.weight.r#type = FWP_UINT8;
    f.weight.Anonymous.uint8 = weight;
    f.numFilterConditions = cs.len() as u32;
    f.filterCondition = cs.as_mut_ptr();
    f.action.r#type = action;
    let mut id = 0u64;
    let rc = FwpmFilterAdd0(h, &f, null_mut(), &mut id);
    r("wfp", format!("add {name} rc={rc:#x} id={id}"));
    drop(keep);
    if rc == 0 { Ok(()) } else { Err(format!("FwpmFilterAdd0({name}) rc {rc:#x}")) }
}

unsafe fn engine() -> Res<HANDLE> {
    let mut h: HANDLE = null_mut();
    let rc = FwpmEngineOpen0(null(), 10, null(), null(), &mut h);
    if rc != 0 {
        return Err(format!("FwpmEngineOpen0 rc {rc:#x}"));
    }
    Ok(h)
}

pub unsafe fn install(a: &[String]) -> Res<()> {
    let port: u16 = opt(a, "--port").and_then(|s| s.parse().ok()).ok_or("--port")?;
    let m = match (opt(a, "--sd"), opt(a, "--sid")) {
        (Some(sd), _) => Cond::UserSd(sd),
        (None, Some(sid)) => Cond::Package(sid),
        _ => return Err("--sd or --sid".into()),
    };
    let same = |m: &Cond| match m {
        Cond::UserSd(s) => Cond::UserSd(s.clone()),
        Cond::Package(s) => Cond::Package(s.clone()),
        _ => unreachable!(),
    };
    let h = engine()?;
    let rc = FwpmTransactionBegin0(h, 0);
    r("wfp", format!("transaction begin rc={rc:#x}"));
    let mut pname = wide("kithbox-probe");
    let mut p: FWPM_PROVIDER0 = std::mem::zeroed();
    p.providerKey = PROVIDER;
    p.displayData.name = pname.as_mut_ptr();
    p.flags = FWPM_PROVIDER_FLAG_PERSISTENT;
    r("wfp", format!("provider rc={:#x}", FwpmProviderAdd0(h, &p, null_mut())));
    let mut sname = wide("kithbox-probe-sublayer");
    let mut provider = PROVIDER;
    let mut sl: FWPM_SUBLAYER0 = std::mem::zeroed();
    sl.subLayerKey = SUBLAYER;
    sl.displayData.name = sname.as_mut_ptr();
    sl.flags = FWPM_SUBLAYER_FLAG_PERSISTENT;
    sl.providerKey = &mut provider;
    sl.weight = 0x4000;
    r("wfp", format!("sublayer rc={:#x}", FwpmSubLayerAdd0(h, &sl, null_mut())));
    let (v4, v6) = (FWPM_LAYER_ALE_AUTH_CONNECT_V4, FWPM_LAYER_ALE_AUTH_CONNECT_V6);
    let block = FWP_ACTION_BLOCK;
    let mut res = Vec::new();
    res.push(add(
        h,
        "kithbox-loopback-but-port",
        1,
        v4,
        10,
        block,
        &[same(&m), Cond::Range(0x7F00_0000, 0x7FFF_FFFF), Cond::PortNot(port)],
    ));
    res.push(add(h, "kithbox-below-loopback", 2, v4, 10, block, &[same(&m), Cond::Range(0, 0x7EFF_FFFF)]));
    res.push(add(h, "kithbox-above-loopback", 3, v4, 10, block, &[same(&m), Cond::Range(0x8000_0000, 0xFFFF_FFFF)]));
    res.push(add(h, "kithbox-v6", 4, v6, 10, block, &[same(&m)]));
    if let Some(sd) = opt(a, "--permit-sd") {
        // owner processes: a higher weight in the same sublayer decides first
        res.push(add(h, "kithbox-permit-owner-v4", 5, v4, 12, FWP_ACTION_PERMIT, &[Cond::UserSd(sd.clone())]));
        res.push(add(h, "kithbox-permit-owner-v6", 6, v6, 12, FWP_ACTION_PERMIT, &[Cond::UserSd(sd)]));
    }
    r("wfp", format!("transaction commit rc={:#x}", FwpmTransactionCommit0(h)));
    FwpmEngineClose0(h);
    res.into_iter().collect::<Res<Vec<()>>>().map(|_| ())
}

pub unsafe fn remove() -> Res<()> {
    let h = engine()?;
    for i in 1..=6 {
        r("wfp", format!("delete filter {i} rc={:#x}", FwpmFilterDeleteByKey0(h, &fkey(i))));
    }
    r("wfp", format!("delete sublayer rc={:#x}", FwpmSubLayerDeleteByKey0(h, &SUBLAYER)));
    r("wfp", format!("delete provider rc={:#x}", FwpmProviderDeleteByKey0(h, &PROVIDER)));
    FwpmEngineClose0(h);
    Ok(())
}
