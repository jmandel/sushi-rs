//! Q6 setIds / generateIds (PU:4285/4339) + SliceList (PU:4307). Regenerates
//! every element id from path + slice analysis. `id == path` when no slices.

use serde_json::Value;

use super::paths::path_of;

/// SliceList (PU:4307): tracks the active sliceName at each path depth.
#[derive(Default)]
struct SliceList {
    slices: Vec<(String, String)>, // (path, sliceName), insertion-ordered
}

impl SliceList {
    fn see_element(&mut self, ed: &Value) {
        let path = path_of(ed);
        self.slices
            .retain(|(k, _)| !(k.len() > path.len() || k == path));
        if let Some(name) = ed.get("sliceName").and_then(Value::as_str) {
            self.slices.push((path.to_string(), name.to_string()));
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.slices
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn analyse(&self, paths: &[&str]) -> Vec<Option<String>> {
        let mut res = vec![None; paths.len()];
        let mut s = paths[0].to_string();
        for i in 1..paths.len() {
            s.push('.');
            s.push_str(paths[i]);
            res[i] = self.get(&s).map(str::to_string);
        }
        res
    }
}

fn fix_chars(s: &str) -> String {
    s.replace('_', "-")
}

/// PU:4339 generateIds — assigns `id` to every element in `list`. `type_name`
/// is the resource type; used to absolutize `#`-prefixed contentReference
/// (PU:4388) as `http://hl7.org/fhir/StructureDefinition/<type>#<frag>`.
pub(crate) fn generate_ids(list: &mut [Value]) {
    generate_ids_with_type(list, None)
}

pub(crate) fn generate_ids_with_type(list: &mut [Value], type_name: Option<&str>) {
    if list.is_empty() {
        return;
    }
    let mut slice_info = SliceList::default();
    for ed in list.iter_mut() {
        let path = path_of(ed).to_string();
        slice_info.see_element(ed);
        let pl: Vec<&str> = path.split('.').collect();
        let slices = slice_info.analyse(&pl);
        let mut b = String::from(pl[0]);
        for i in 1..pl.len() {
            b.push('.');
            b.push_str(&fix_chars(pl[i]));
            if let Some(p) = &slices[i] {
                b.push(':');
                b.push_str(p);
            }
        }
        let cref = ed
            .get("contentReference")
            .and_then(Value::as_str)
            .filter(|c| c.starts_with('#'))
            .map(str::to_string);
        if let Some(obj) = ed.as_object_mut() {
            obj.insert("id".to_string(), Value::String(b));
            if let (Some(cref), Some(type_name)) = (cref, type_name) {
                let type_url = format!("http://hl7.org/fhir/StructureDefinition/{type_name}");
                obj.insert(
                    "contentReference".to_string(),
                    Value::String(format!("{type_url}{cref}")),
                );
            }
        }
    }
}
