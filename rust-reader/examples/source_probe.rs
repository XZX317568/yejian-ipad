//! Explicit live diagnostic; never runs in the normal test suite.
//! cargo run --example source_probe -- <downloaded source catalogue>
use yejian_sources::*;

fn main() -> anyhow::Result<()> {
    let root = std::env::args().nth(1).ok_or_else(||anyhow::anyhow!("catalogue directory required"))?;
    let catalogue=SourceCatalog::new(root);
    for source in catalogue.list()? {
        let mut record=serde_json::json!({"source":source.manifest.id,"version":source.manifest.version});
        eprintln!("Probing {}",source.manifest.id);
        let engine=match SourceEngine::load_installed(&source) {
            Ok(engine)=>engine,
            Err(error)=>{record["load_error"]=error.to_string().into();println!("{}",record);continue;}
        };
        let filters=pollster::block_on(engine.filters());
        match &filters {
            Ok(filters)=>{
                record["filter_count"]=filters.len().into();
                record["filter_titles"]=filters.iter().map(|filter|
                    serde_json::Value::String(filter.title.clone().unwrap_or(filter.id.clone()))).collect();
            }
            Err(error)=>record["filter_error"]=error.to_string().into(),
        }
        match pollster::block_on(engine.listings()) {
            Ok(listings)=>record["listings"]=listings.iter().map(|listing|
                serde_json::json!({"id":listing.id,"name":listing.name})).collect(),
            Err(error)=>record["listing_error"]=error.to_string().into(),
        }
        // One real selection from the schema, never an invented genre/language.
        let selected=filters.as_ref().ok().and_then(|filters|filters.iter().find_map(|filter|{
            if let FilterTypeConfig::Select(config)=&filter.config {
                let defaults=default_filter_values(std::slice::from_ref(filter));
                config.options.iter().enumerate().find_map(|(index,option)|{
                    let value=config.ids.as_ref().and_then(|ids|ids.get(index)).unwrap_or(option);
                    let candidate=FilterValue::Select{id:filter.id.clone(),value:value.clone()};
                    if defaults.contains(&candidate){None}else{Some(candidate)}
                })
            } else{None}
        }));
        let values=selected.into_iter().collect::<Vec<_>>();
        record["submitted_filters"]=serde_json::to_value(&values)?;
        match pollster::block_on(engine.search_page("",1,values)) {
            Ok(page)=>{
                record["result_count"]=page.entries.len().into();
                record["has_next"]=page.has_next_page.into();
                record["sample_keys"]=page.entries.iter().take(3).map(|m|serde_json::Value::String(m.key.clone())).collect();
            }
            Err(error)=>record["search_error"]=error.to_string().into(),
        }
        println!("{}",record);
    }
    Ok(())
}
