use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Item {
    pub code: String,
    pub title: String,
    pub norm_reference: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct WorkType {
    pub name: String,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct CommonError {
    pub code: String,
    pub title: String,
    pub description: String,
    pub algorithm: String,
    pub check_type: String,
    pub severity: String,
}
#[derive(Clone)]
pub struct Catalog {
    pub work_types: Vec<WorkType>,
    pub errors: Vec<CommonError>,
}
pub fn normalize(s: &str) -> String { s.to_lowercase().replace('ё', "е") }
impl Catalog {
    pub fn load() -> anyhow::Result<Self> {
        Ok(Self {
            work_types: serde_json::from_str(include_str!("../catalog/work_types.json"))?,
            errors: serde_json::from_str(include_str!("../catalog/common_errors.json"))?,
        })
    }
    pub fn work_type(&self, name: &str) -> Option<&WorkType> {
        self.work_types.iter().find(|t| t.name == name)
    }
    pub fn closest(&self, name: &str) -> Option<&WorkType> {
        self.work_types.iter().max_by(|a,b| {
            strsim::normalized_levenshtein(&normalize(name),&normalize(&a.name))
                .total_cmp(&strsim::normalized_levenshtein(&normalize(name),&normalize(&b.name)))
        }).filter(|t| strsim::normalized_levenshtein(&normalize(name), &normalize(&t.name)) >= 0.85)
    }
    pub fn item(&self, work: &str, code: &str) -> Option<&Item> {
        self.work_type(work)?.items.iter().find(|i| i.code == code)
    }
    pub fn classify(&self, filename: &str, text: &str, work: Option<&WorkType>) -> Option<(String,String)> {
        let work = work?;
        let hay = normalize(&format!("{}\n{}",filename,text.chars().take(3500).collect::<String>()));
        let aliases = [
            ("реестр исполнительной документации", vec!["реестр исполнительной документации"]),
            ("акты освидетельствования скрытых работ",vec!["освидетельствования скрытых работ", "аоср"]),
            ("акт разбивки осей",vec!["разбивки осей"]),
            ("исполнительные геодезические схемы",vec!["исполнительная геодезическая схема", "исполнительные геодезические схемы"]),
            ("исполнительные схемы",vec!["исполнительная схема", "исполнительные схемы"]),
        ];
        for (title, phrases) in aliases {
            for item in &work.items {
                if normalize(&item.title).contains(title) && phrases.iter().any(|s| hay.contains(s)) {
                    return Some((item.code.clone(),item.title.clone()));
                }
            }
        }
        let probe = normalize(filename.rsplit_once('.').map(|(s,_)| s).unwrap_or(filename))
            .replace('_', " ").replace('-', " ");
        let mut ranked: Vec<_> = work.items.iter().map(|i| (strsim::normalized_levenshtein(&probe,&normalize(&i.title)),i)).collect();
        ranked.sort_by(|a,b| b.0.total_cmp(&a.0));
        if let Some((score, item)) = ranked.first() {
            let runner_up = ranked.get(1).map(|(s,_)| *s).unwrap_or(0.0);
            if *score >= 0.88 && *score - runner_up >= 0.08 {
                return Some((item.code.clone(),item.title.clone()));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imported_sources() {
        let c=Catalog::load().unwrap();
        assert_eq!(c.work_types.len(), 14);
        assert_eq!(c.work_types.iter().map(|w|w.items.len()).sum::<usize>(),169);
        assert_eq!(c.errors.len(),34);
    }
    #[test]
    fn earthworks_and_reestr() {
        let c=Catalog::load().unwrap();
        let w=c.closest("Земляные работы").unwrap();
        assert_eq!(c.classify("Реестр исполнительной документации.xlsx", "", Some(w)).unwrap().0,"1.1");
    }
}
