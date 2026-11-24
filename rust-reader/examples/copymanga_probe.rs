//! Live compatibility probe for the real zh.copymanga (拷贝漫画) source.
//! Explicit network diagnostic — never part of the test suite:
//!   cargo run --locked -j1 --example copymanga_probe -- ../.tools/copymanga-catalog
//! The argument must be a dedicated test catalogue (created when missing,
//! reused when it already holds zh.copymanga) — never user data. Every step
//! reports progress on stderr, failures collect into `errors`, exit code 1.
use yejian_sources::*;

const INDEX_URL:&str="https://aidoku-community.github.io/sources/index.min.json";
const SOURCE_ID:&str="zh.copymanga";
const QUERY:&str="海贼王";

fn step(text:impl std::fmt::Display){eprintln!("[copymanga_probe] {text}");}
fn fail(errors:&mut Vec<serde_json::Value>,step:&str,error:impl std::fmt::Display){
    eprintln!("[copymanga_probe] {step} FAILED: {error}");
    errors.push(serde_json::json!({"step":step,"error":error.to_string()}));
}
fn done(record:&mut serde_json::Value,errors:Vec<serde_json::Value>)->i32{
    let ok=errors.is_empty();
    record["errors"]=errors.into();
    record["ok"]=ok.into();
    if ok{0}else{1}
}

fn main(){
    let mut record=serde_json::json!({"source":SOURCE_ID});
    let code=probe(&mut record);
    println!("{record}");
    std::process::exit(code);
}

fn probe(record:&mut serde_json::Value)->i32{
    let mut errors=Vec::new();
    macro_rules! bail{($s:expr,$e:expr)=>{{fail(&mut errors,$s,$e);return done(record,errors);}}}
    let Some(dir)=std::env::args().nth(1) else{bail!("args","a dedicated test catalogue directory is required")};
    step(format!("catalogue {dir}"));
    let installer=match SourceInstaller::new(SourceCatalog::new(dir)){
        Ok(installer)=>installer,Err(e)=>bail!("installer",e)};
    // Reuse an existing install inside this dedicated catalogue; otherwise
    // fetch the official Aidoku community index and install zh.copymanga.
    let installed=if installer.catalog().is_installed(SOURCE_ID){
        step("reuse installed zh.copymanga");
        installer.catalog().get(SOURCE_ID).map_err(|e|e.to_string())
    }else{
        step("fetch index + install zh.copymanga");
        installer.fetch_index(INDEX_URL)
            .and_then(|index|installer.install(&index,SOURCE_ID)).map_err(|e|e.to_string())
    };
    let installed=match installed{Ok(source)=>source,Err(e)=>bail!("install",e)};
    record["name"]=installed.manifest.name.clone().into();
    record["version"]=installed.manifest.version.into();
    step("load engine");
    let engine=match SourceEngine::load_installed(&installed){
        Ok(engine)=>engine,Err(e)=>bail!("load",e)};

    step("filters schema");
    let schema=match pollster::block_on(engine.filters()){
        Ok(filters)=>{record["filter_count"]=filters.len().into();filters}
        Err(e)=>{fail(&mut errors,"filters",e);Vec::new()}
    };
    let selected=pick_non_default(&schema);
    record["submitted_filters"]=serde_json::to_value(&selected).unwrap_or_default();

    step("listings");
    match pollster::block_on(engine.listings()){
        Ok(listings)=>{
            record["listings"]=listings.iter()
                .map(|l|serde_json::json!({"id":l.id,"name":l.name})).collect();
            let mut results=Vec::new();
            for listing in &listings{
                step(format!("browse listing {}",listing.id));
                let mut result=serde_json::json!({"id":listing.id,"name":listing.name});
                match pollster::block_on(engine.browse(&listing.id,1)){
                    Ok(page)=>{
                        result["count"]=page.entries.len().into();
                        result["has_next"]=page.has_next_page.into();
                        if page.entries.is_empty(){fail(&mut errors,"listing",format!("\"{}\" empty",listing.id))}
                    }
                    Err(e)=>{
                        result["error"]=e.to_string().into();
                        fail(&mut errors,"listing",format!("\"{}\": {e}",listing.id))
                    }
                }
                results.push(result);
            }
            record["listing_results"]=results.into();
        }
        Err(e)=>fail(&mut errors,"listings",e),
    }

    step(format!("search {QUERY}"));
    let first=match pollster::block_on(engine.search_page(QUERY,1,Vec::new())){
        Ok(page)=>{
            record["search_count"]=page.entries.len().into();
            if page.entries.is_empty(){fail(&mut errors,"search","empty result")}
            page.entries.into_iter().next()
        }
        Err(e)=>{fail(&mut errors,"search",e);None}
    };
    step("filtered search (empty query + non-default filter)");
    match pollster::block_on(engine.search_page("",1,selected)){
        Ok(page)=>{
            record["filtered_count"]=page.entries.len().into();
            if page.entries.is_empty(){fail(&mut errors,"filtered_search","empty result")}
        }
        Err(e)=>fail(&mut errors,"filtered_search",e),
    }

    let Some(manga)=first else{return done(record,errors)};
    record["manga_key"]=manga.key.clone().into();
    record["manga_title"]=manga.title.clone().into();
    step("manga details");
    match pollster::block_on(engine.manga_details(&manga.key)){
        Ok(manga)=>record["detail_title"]=manga.title.into(),
        Err(e)=>fail(&mut errors,"details",e),
    }
    step("chapters");
    match pollster::block_on(engine.chapters(&manga.key)){
        Ok(chapters)=>{
            let total=chapters.len();
            let unnamed=chapters.iter().filter(|c|c.title.is_none()).count();
            let unnumbered=chapters.iter().filter(|c|c.chapter_number.is_none()).count();
            // Readable label = non-blank title, or a finite chapter/volume
            // number (the UI falls back to 第 N 话 / 第 N 卷, then the key).
            let unlabeled=|c:&Chapter|c.title.as_deref().map_or(true,|t|t.trim().is_empty())
                && !c.chapter_number.is_some_and(f32::is_finite)
                && !c.volume_number.is_some_and(f32::is_finite);
            let label_unavailable=chapters.iter().filter(|c|unlabeled(c)).count();
            // Volume-only chapters are the ones whose visible name can only
            // come from the volume fallback; record which real numbers they
            // carry instead of assuming the last chapter's volume is typical.
            let volume_only=|c:&Chapter|c.title.as_deref().map_or(true,|t|t.trim().is_empty())
                && !c.chapter_number.is_some_and(f32::is_finite)
                && c.volume_number.is_some_and(f32::is_finite);
            let volume_only_count=chapters.iter().filter(|c|volume_only(c)).count();
            let distinct_volume_only_numbers:std::collections::BTreeSet<String>=chapters.iter()
                .filter(|c|volume_only(c)).filter_map(|c|c.volume_number.map(|v|format!("{v}"))).collect();
            record["chapter_count"]=total.into();
            record["chapter_title_null_ratio"]=(unnamed as f64/total.max(1) as f64).into();
            record["chapter_number_null_ratio"]=(unnumbered as f64/total.max(1) as f64).into();
            record["chapter_label_unavailable_count"]=label_unavailable.into();
            record["volume_only_count"]=volume_only_count.into();
            record["distinct_volume_only_numbers"]=serde_json::json!(distinct_volume_only_numbers);
            if chapters.is_empty(){fail(&mut errors,"chapters","no chapters")}
            let sample=|chapter:&Chapter|serde_json::json!({"key":chapter.key,"title":chapter.title,
                "chapter_number":chapter.chapter_number,"volume_number":chapter.volume_number});
            let mut indices=vec![0,total/2,total.saturating_sub(1)];
            indices.dedup();
            record["chapter_samples"]=indices.iter().filter_map(|i|chapters.get(*i).map(&sample)).collect();
            // A source returning key fallback as the only visible name would
            // otherwise look healthy here: all title+number+volume unreadable
            // is diagnostic.
            if total>0&&label_unavailable==total{
                fail(&mut errors,"chapters","all chapters only have keys (title/number/volume unreadable)");
            }
            if let Some(chapter)=chapters.first(){
                record["first_chapter_key"]=chapter.key.clone().into();
                step("pages of first chapter");
                match pollster::block_on(engine.pages_for(&manga.key,&chapter.key)){
                    Ok(pages)=>{
                        record["page_count"]=pages.len().into();
                        if pages.is_empty(){fail(&mut errors,"pages","no pages")}
                        image_step(&engine,&pages,record,&mut errors);
                    }
                    Err(e)=>fail(&mut errors,"pages",e),
                }
            }
        }
        Err(e)=>fail(&mut errors,"chapters",e),
    }
    done(record,errors)
}

/// One non-default option from a real `select` filter, exactly like
/// `source_probe` — schema-provided values only, never invented.
fn non_default(filter:&Filter)->Option<FilterValue>{
    let FilterTypeConfig::Select(config)=&filter.config else{return None};
    let defaults=default_filter_values(std::slice::from_ref(filter));
    config.options.iter().enumerate().find_map(|(index,option)|{
        let value=config.ids.as_ref().and_then(|ids|ids.get(index)).unwrap_or(option);
        let candidate=FilterValue::Select{id:filter.id.clone(),value:value.clone()};
        if defaults.contains(&candidate){None}else{Some(candidate)}
    })
}

/// Prefer the real 题材/theme select (its schema id is Chinese in some
/// releases), then `id == "theme"`, then any select. Schema values only.
fn pick_non_default(filters:&[Filter])->Vec<FilterValue>{
    filters.iter().find(|filter|filter.id=="题材"||filter.title.as_deref()==Some("题材")).and_then(non_default)
        .or_else(||filters.iter().find(|filter|filter.id=="theme").and_then(non_default))
        .or_else(||filters.iter().find_map(non_default)).into_iter().collect()
}

/// Downloads the first page image through the source's own HTTP session
/// (`image_request` for signing/headers, then the engine request handler) —
/// never a bare reqwest call that would bypass source cookies/headers.
fn image_step(engine:&SourceEngine,pages:&[Page],record:&mut serde_json::Value,
    errors:&mut Vec<serde_json::Value>){
    let Some((url,context))=pages.iter().find_map(|page|match &page.content{
        PageContent::Url{url,context}=>Some((url.clone(),context.clone())),
        PageContent::ZipFile{url,..}=>Some((url.clone(),None)),
        _=>None,
    }) else{
        fail(errors,"image","no URL page content in first chapter");return;
    };
    record["first_image_url"]=url.clone().into();
    let mut request=NetRequestData{
        method:"GET".into(),url,headers:Vec::new(),body:None,timeout_ms:30_000};
    step("resolve image request");
    match pollster::block_on(engine.image_request(&request.url,context)){
        Ok(Some(resolved))=>{
            request.url=resolved.url;request.headers=resolved.headers;request.body=resolved.body}
        Ok(None)=>{}
        Err(e)=>fail(errors,"image_request",e),
    }
    step("fetch first image");
    let response=engine.request_handler()(request);
    match response.error{
        Some(e)=>fail(errors,"image_fetch",e),
        None=>{
            record["image_status"]=response.status_code.into();
            record["image_bytes"]=response.body.len().into();
            if !(200..=299).contains(&response.status_code){
                fail(errors,"image_fetch",format!("HTTP {}",response.status_code));
            }else if response.body.is_empty(){
                fail(errors,"image_fetch","empty image body");
            }
        }
    }
}
