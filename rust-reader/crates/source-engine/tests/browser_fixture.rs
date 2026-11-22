//! `tests/fixtures/browser/` — a self-contained Aidoku source used for real
//! UI/device acceptance of the filters feature, plus its offline tests.
//!
//! The fixture source is named `筛选验收源` ("filter acceptance source") so it
//! can never be mistaken for a real external source. Its `get_search_manga_list`
//! compares the *exact* postcard-encoded `FilterValue` it receives against a
//! table of pre-encoded needles and returns a title that proves which value
//! arrived; `get_manga_list` returns distinct titles for the two static
//! listings and for page 2. `get_manga_update` supplies two offline chapters
//! for the production detail → downloaded reading → return flow.
//!
//! The three fixture files are committed test assets, not build output.
//! `build_fixture` is an explicit regenerator (run it after changing the WAT
//! template or JSON):  `cargo test --test browser_fixture build_fixture --
//! --ignored`. `fixture_files_match_generator` then fails if the committed
//! files drift from the generator, and the acceptance tests only read them.

use std::fs;
use std::path::{Path, PathBuf};

use yejian_sources::{
    Chapter, FilterValue, InstalledSource, Listing, ListingKind, Manga, MangaPageResult, SortFilterValue,
    SourceEngine, SourceManifest, default_filter_values,
};

// ---------------------------------------------------------------------------
// postcard encoding (little-endian varints, same wire format as the engine)
// ---------------------------------------------------------------------------

fn var_u64(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

fn zigzag32(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}
fn zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    var_u64(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// postcard `list<Manga>` + `bool has_next_page`, wrapped in the `len+8` /
/// 4-byte-pad result block `handle_result` expects.
fn page_result_block(entries: &[Manga], has_next_page: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    var_u64(&mut payload, entries.len() as u64);
    for manga in entries {
        payload.extend_from_slice(&manga.to_postcard_bytes());
    }
    payload.push(has_next_page as u8);
    result_block(&payload)
}

fn result_block(payload: &[u8]) -> Vec<u8> {
    let mut block = Vec::with_capacity(payload.len() + 8);
    block.extend_from_slice(&((payload.len() + 8) as u32).to_le_bytes());
    block.extend_from_slice(&[0u8; 4]);
    block.extend_from_slice(payload);
    block
}

fn manga(key: &str, title: &str) -> Manga {
    Manga {
        key: key.into(),
        title: title.into(),
        cover: Some("https://fixture.invalid/cover.png".into()),
        ..Default::default()
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/browser")
}

// ---------------------------------------------------------------------------
// Exact-encoded single-value needles
// ---------------------------------------------------------------------------
//
// The engine encodes `filters` as `list<FilterValue>`; a single submitted
// filter therefore produces `01 <tag u8> <fields…>`. The fixture's WASM only
// needs `std.buffer_len` + `std.read_buffer` + a byte-wise `memcmp` — no
// generic decoder — because each needle is the complete expected descriptor.

/// Encodes one `FilterValue` exactly as `FilterValue::to_postcard` does.
fn filter_value_bytes(v: &FilterValue) -> Vec<u8> {
    let mut out = Vec::new();
    match v {
        FilterValue::Text { id, value } => {
            out.push(0);
            write_string(&mut out, id);
            write_string(&mut out, value);
        }
        FilterValue::Sort(s) => {
            out.push(1);
            write_string(&mut out, &s.id);
            var_u64(&mut out, zigzag32(s.index) as u64);
            out.push(s.ascending as u8);
        }
        FilterValue::Check { id, value } => {
            out.push(2);
            write_string(&mut out, id);
            var_u64(&mut out, zigzag64(*value));
        }
        FilterValue::Select { id, value } => {
            out.push(3);
            write_string(&mut out, id);
            write_string(&mut out, value);
        }
        FilterValue::MultiSelect {
            id,
            included,
            excluded,
        } => {
            out.push(4);
            write_string(&mut out, id);
            var_u64(&mut out, included.len() as u64);
            for s in included {
                write_string(&mut out, s);
            }
            var_u64(&mut out, excluded.len() as u64);
            for s in excluded {
                write_string(&mut out, s);
            }
        }
        FilterValue::Range { id, from, to } => {
            out.push(5);
            write_string(&mut out, id);
            write_f32_opt(&mut out, from);
            write_f32_opt(&mut out, to);
        }
    }
    out
}

fn write_f32_opt(out: &mut Vec<u8>, v: &Option<f32>) {
    match v {
        None => out.push(0),
        Some(f) => {
            out.push(1);
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
}

/// A `list<FilterValue>` containing exactly one filter — the needle the WASM
/// compares the incoming descriptor against byte-for-byte.
fn single_filter_desc(v: &FilterValue) -> Vec<u8> {
    let mut out = vec![1u8]; // list len = 1
    out.extend_from_slice(&filter_value_bytes(v));
    out
}

// ---------------------------------------------------------------------------
// WAT
// ---------------------------------------------------------------------------

struct Case {
    needle_desc: Vec<u8>,
    result: Vec<u8>,
}

fn fixture_wasm() -> Vec<u8> {
    // Each case: an exact single-FilterValue descriptor -> the result to
    // return. Titles are deliberately visible for UI acceptance.
    let cases: Vec<Case> = vec![
        Case {
            needle_desc: single_filter_desc(&FilterValue::Check {
                id: "completed".into(),
                value: 1,
            }),
            result: page_result_block(&[manga("fx-done-inc", "筛选:已完结(含)")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::Check {
                id: "completed".into(),
                value: 2,
            }),
            result: page_result_block(&[manga("fx-done-exc", "筛选:排除完结")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::Select {
                id: "genre".into(),
                value: "adv".into(),
            }),
            result: page_result_block(&[manga("fx-adv", "筛选:冒险题材")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::Sort(SortFilterValue {
                id: "order".into(),
                index: 1,
                ascending: false,
            })),
            result: page_result_block(&[manga("fx-sort-desc", "筛选:最新降序")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::Sort(SortFilterValue {
                id: "order".into(),
                index: 1,
                ascending: true,
            })),
            result: page_result_block(&[manga("fx-sort-asc", "筛选:最新升序")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::MultiSelect {
                id: "tags".into(),
                included: vec!["heal".into()],
                excluded: vec![],
            }),
            result: page_result_block(&[manga("fx-heal-inc", "筛选:含治愈")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::MultiSelect {
                id: "tags".into(),
                included: vec![],
                excluded: vec!["heal".into()],
            }),
            result: page_result_block(&[manga("fx-heal-exc", "筛选:排除治愈")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::Range {
                id: "year".into(),
                from: Some(2000.0),
                to: None,
            }),
            result: page_result_block(&[manga("fx-year", "筛选:年份2000起")], false),
        },
        Case {
            needle_desc: single_filter_desc(&FilterValue::Text {
                id: "title".into(),
                value: "one piece".into(),
            }),
            result: page_result_block(&[manga("fx-text", "筛选:标题关键词")], false),
        },
    ];

    let plain = page_result_block(&[manga("fx-plain", "未筛选结果")], false);
    let multi = page_result_block(&[manga("fx-multi", "筛选:多条件")], false);
    let pop1 = page_result_block(&[manga("fx-pop-1", "热门-第1页")], true);
    let pop2 = page_result_block(&[manga("fx-pop-2", "热门-第2页")], false);
    let lat1 = page_result_block(&[manga("fx-lat-1", "最新-第1页")], true);
    // This is an incremental update: blank keys/titles preserve the selected
    // listing result when the engine merges it. Return newest first so the
    // app's reading-order control must do actual work during acceptance.
    let detail = result_block(&Manga {
        description: Some("本地日用验收示例，章节图片来自临时样书。".into()),
        chapters: Some([2, 1].into_iter().map(|number| Chapter {
            key: format!("daily-{number}"),
            title: Some(format!("第 {number} 话")),
            chapter_number: Some(number as f32),
            ..Default::default()
        }).collect()),
        ..Default::default()
    }.to_postcard_bytes());

    // listing-name needles for get_manga_list
    let n_popular = b"popular";
    let n_latest = b"latest";

    // ---- memory layout ----
    let scratch = 4096usize;
    let mut cursor = 8192usize;
    let mut segs: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut place = |bytes: &[u8], segs: &mut Vec<(usize, Vec<u8>)>| {
        let at = cursor;
        cursor += bytes.len();
        segs.push((at, bytes.to_vec()));
        at
    };

    let p_plain = place(&plain, &mut segs);
    let p_multi = place(&multi, &mut segs);
    let p_pop1 = place(&pop1, &mut segs);
    let p_pop2 = place(&pop2, &mut segs);
    let p_lat1 = place(&lat1, &mut segs);
    let p_detail = place(&detail, &mut segs);
    // each case's needle descriptor + its result block
    let mut case_offsets = Vec::new();
    for c in &cases {
        let nd = place(&c.needle_desc, &mut segs);
        let rp = place(&c.result, &mut segs);
        case_offsets.push((nd, c.needle_desc.len(), rp));
    }
    let np_off = place(n_popular, &mut segs);
    let nl_off = place(n_latest, &mut segs);

    let esc = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("\\{b:02x}")).collect() };
    let data: String = segs
        .iter()
        .map(|(at, bytes)| format!("(data (i32.const {at}) \"{}\")", esc(bytes)))
        .collect::<Vec<_>>()
        .join("\n            ");

    // WAT for the per-case memcmp dispatch.
    let mut search_body = String::new();
    for (nd, len, rp) in &case_offsets {
        search_body.push_str(&format!(
            "(if (call $memeq (i32.const {scratch}) (local.get $flen)\n                                    (i32.const {nd}) (i32.const {len}))\n                    (then (return (i32.const {rp}))))\n                "
        ));
    }
    let n_cases = case_offsets.len();

    let source = format!(
        r#"(module
            (import "std" "buffer_len" (func $len (param i32) (result i32)))
            (import "std" "read_buffer" (func $read (param i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            {data}

            ;; memeq(a_off, a_len, b_off, b_len) -> 1 when equal byte-for-byte
            (func $memeq
                (param $a i32) (param $alen i32) (param $b i32) (param $blen i32)
                (result i32)
                (local $i i32)
                (if (i32.ne (local.get $alen) (local.get $blen))
                    (then (return (i32.const 0))))
                (loop $cmp
                    (if (i32.ge_u (local.get $i) (local.get $alen))
                        (then (return (i32.const 1))))
                    (if (i32.ne
                            (i32.load8_u (i32.add (local.get $a) (local.get $i)))
                            (i32.load8_u (i32.add (local.get $b) (local.get $i))))
                        (then (return (i32.const 0))))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $cmp))
                (i32.const 0))

            ;; contains(hay, haylen, ndl, ndllen) -> 0/1  (substring, for listings)
            (func $contains
                (param $hay i32) (param $hay_len i32)
                (param $ndl i32) (param $ndl_len i32) (result i32)
                (local $i i32) (local $j i32) (local $limit i32)
                (local.set $limit (i32.sub (local.get $hay_len) (local.get $ndl_len)))
                (block $done
                    (br_if $done (i32.lt_s (local.get $limit) (i32.const 0)))
                    (loop $start
                        (br_if $done (i32.gt_s (local.get $i) (local.get $limit)))
                        (local.set $j (i32.const 0))
                        (block $next_start
                            (loop $cmp
                                (if (i32.ge_u (local.get $j) (local.get $ndl_len))
                                    (then (return (i32.const 1))))
                                (br_if $next_start
                                    (i32.ne
                                        (i32.load8_u (i32.add (local.get $hay)
                                            (i32.add (local.get $i) (local.get $j))))
                                        (i32.load8_u (i32.add (local.get $ndl)
                                            (local.get $j)))))
                                (local.set $j (i32.add (local.get $j) (i32.const 1)))
                                (br $cmp)))
                        (local.set $i (i32.add (local.get $i) (i32.const 1)))
                        (br $start)))
                (i32.const 0))

            ;; get_search_manga_list(query_desc, page, filters_desc)
            (func (export "get_search_manga_list")
                (param $q i32) (param $page i32) (param $f i32) (result i32)
                (local $flen i32)
                (local.set $flen (call $len (local.get $f)))
                ;; empty list encodes as a single varint byte; >1 filter or a
                ;; combination we don't recognise -> "多条件"
                (if (i32.le_s (local.get $flen) (i32.const 1))
                    (then (return (i32.const {p_plain}))))
                (drop (call $read (local.get $f) (i32.const {scratch}) (local.get $flen)))
                {search_body}
                ;; a descriptor longer than one filter, or an unknown value
                (i32.const {p_multi}))

            ;; get_manga_list(listing_desc, page)
            (func (export "get_manga_list")
                (param $l i32) (param $page i32) (result i32)
                (local $llen i32)
                (local.set $llen (call $len (local.get $l)))
                (drop (call $read (local.get $l) (i32.const {scratch}) (local.get $llen)))
                (if (call $contains (i32.const {scratch}) (local.get $llen)
                                    (i32.const {np_off}) (i32.const {np_len}))
                    (then
                        (if (i32.ge_s (local.get $page) (i32.const 2))
                            (then (return (i32.const {p_pop2})))
                            (else (return (i32.const {p_pop1}))))))
                (if (call $contains (i32.const {scratch}) (local.get $llen)
                                    (i32.const {nl_off}) (i32.const {nl_len}))
                    (then (return (i32.const {p_lat1}))))
                (i32.const {p_plain}))

            ;; get_manga_update(manga_desc, needs_details, needs_chapters)
            (func (export "get_manga_update")
                (param i32 i32 i32) (result i32)
                (i32.const {p_detail})))"#,
        np_len = n_popular.len(),
        nl_len = n_latest.len(),
    );
    // silence the unused-variable lint when n_cases changes shape
    let _ = n_cases;
    wat::parse_str(source).expect("fixture wat compiles")
}

// ---------------------------------------------------------------------------
// Fixture file contents
// ---------------------------------------------------------------------------

const FIXTURE_SOURCE_JSON: &str = r##"{
    "info": {
        "id": "local.filter-acceptance",
        "name": "筛选验收源",
        "version": 1,
        "languages": ["zh"],
        "urls": ["https://fixture.invalid"]
    },
    "listings": [
        {"id": "popular", "name": "热门", "kind": 0},
        {"id": "latest", "name": "最新", "kind": 0}
    ]
}
"##;

const FIXTURE_FILTERS_JSON: &str = r##"[
    {"id": "title", "type": "text", "title": "标题", "placeholder": "作品名"},
    {"id": "order", "type": "sort", "title": "排序",
     "options": ["相关", "最新", "人气"],
     "default": {"index": 0, "ascending": false}},
    {"id": "completed", "type": "check", "title": "状态", "name": "已完结",
     "canExclude": true, "default": false},
    {"id": "genre", "type": "select", "title": "题材",
     "options": ["全部", "冒险", "恋爱"], "ids": ["", "adv", "rom"],
     "default": ""},
    {"id": "tags", "type": "multi-select", "title": "标签",
     "options": ["热血", "治愈", "悬疑"], "ids": ["hot", "heal", "susp"],
     "canExclude": true, "defaultIncluded": ["hot"]},
    {"id": "note", "type": "note", "text": "筛选条件会写进搜索请求并改变标题"},
    {"id": "year", "type": "range", "title": "年份", "min": 1980, "max": 2030,
     "decimal": false}
]
"##;

// ---------------------------------------------------------------------------
// Generator + tests
// ---------------------------------------------------------------------------

/// Explicit regenerator: `cargo test --test browser_fixture build_fixture --
/// --ignored`. The committed fixture files are inputs to the rest of the
/// suite, so normal test runs must not rewrite them.
#[test]
#[ignore = "explicit regenerator; run with --ignored"]
fn build_fixture() {
    let dir = fixture_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("main.wasm"), fixture_wasm()).unwrap();
    fs::write(dir.join("source.json"), FIXTURE_SOURCE_JSON).unwrap();
    fs::write(dir.join("filters.json"), FIXTURE_FILTERS_JSON).unwrap();
}

/// The committed fixtures must equal what the generator produces — this is a
/// read-only check, so a stale committed file fails loudly instead of being
/// silently rewritten.
#[test]
fn fixture_files_match_generator() {
    let dir = fixture_dir();
    assert_eq!(
        fs::read(dir.join("main.wasm")).expect("run `build_fixture -- --ignored`"),
        fixture_wasm()
    );
    assert_eq!(
        fs::read_to_string(dir.join("source.json")).unwrap(),
        FIXTURE_SOURCE_JSON
    );
    assert_eq!(
        fs::read_to_string(dir.join("filters.json")).unwrap(),
        FIXTURE_FILTERS_JSON
    );
}

fn load_fixture() -> SourceEngine {
    let dir = fixture_dir();
    let manifest = SourceManifest::from_json_str(FIXTURE_SOURCE_JSON).unwrap();
    let installed = InstalledSource {
        manifest,
        directory: dir.clone(),
        wasm_path: dir.join("main.wasm"),
        settings_path: None,
        filters_path: Some(dir.join("filters.json")),
        icon_path: None,
    };
    SourceEngine::load_installed(&installed).unwrap()
}

#[test]
fn fixture_loads_all_seven_filter_kinds() {
    let engine = load_fixture();
    let filters = pollster::block_on(engine.filters()).unwrap();
    assert_eq!(filters.len(), 7);
    let ids: Vec<&str> = filters.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "title",
            "order",
            "completed",
            "genre",
            "tags",
            "note",
            "year"
        ]
    );

    // Resolved-default baselines in schema order (note produces none).
    let values = default_filter_values(&filters);
    assert_eq!(
        values,
        vec![
            FilterValue::Text {
                id: "title".into(),
                value: "".into()
            },
            FilterValue::Sort(SortFilterValue {
                id: "order".into(),
                index: 0,
                ascending: false
            }),
            // check default:false -> exclude (2), NOT neutral
            FilterValue::Check {
                id: "completed".into(),
                value: 2
            },
            // select resolved default -> ids.first when default is present
            FilterValue::Select {
                id: "genre".into(),
                value: "".into()
            },
            FilterValue::MultiSelect {
                id: "tags".into(),
                included: vec!["hot".into()],
                excluded: vec![]
            },
            FilterValue::Range {
                id: "year".into(),
                from: None,
                to: None
            },
        ]
    );
}

#[test]
fn fixture_search_title_proves_exact_filter_bytes() {
    let engine = load_fixture();

    // No filters -> "未筛选结果".
    let page = pollster::block_on(engine.search("")).unwrap();
    assert_eq!(page.entries[0].title, "未筛选结果");

    // check include (1) vs exclude (2) are distinct values, not one substring.
    let inc = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Check {
            id: "completed".into(),
            value: 1,
        }],
    ))
    .unwrap();
    assert_eq!(inc.entries[0].title, "筛选:已完结(含)");
    let exc = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Check {
            id: "completed".into(),
            value: 2,
        }],
    ))
    .unwrap();
    assert_eq!(exc.entries[0].title, "筛选:排除完结");

    // select 冒险
    let page = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Select {
            id: "genre".into(),
            value: "adv".into(),
        }],
    ))
    .unwrap();
    assert_eq!(page.entries[0].title, "筛选:冒险题材");

    // sort ascending vs descending are exact distinct encodings.
    let desc = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Sort(SortFilterValue {
            id: "order".into(),
            index: 1,
            ascending: false,
        })],
    ))
    .unwrap();
    assert_eq!(desc.entries[0].title, "筛选:最新降序");
    let asc = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Sort(SortFilterValue {
            id: "order".into(),
            index: 1,
            ascending: true,
        })],
    ))
    .unwrap();
    assert_eq!(asc.entries[0].title, "筛选:最新升序");

    // multi-select include vs exclude of the same tag.
    let inc = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::MultiSelect {
            id: "tags".into(),
            included: vec!["heal".into()],
            excluded: vec![],
        }],
    ))
    .unwrap();
    assert_eq!(inc.entries[0].title, "筛选:含治愈");
    let exc = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::MultiSelect {
            id: "tags".into(),
            included: vec![],
            excluded: vec!["heal".into()],
        }],
    ))
    .unwrap();
    assert_eq!(exc.entries[0].title, "筛选:排除治愈");

    // a range filter
    let page = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Range {
            id: "year".into(),
            from: Some(2000.0),
            to: None,
        }],
    ))
    .unwrap();
    assert_eq!(page.entries[0].title, "筛选:年份2000起");

    // a text keyword
    let page = pollster::block_on(engine.search_page(
        "",
        1,
        vec![FilterValue::Text {
            id: "title".into(),
            value: "one piece".into(),
        }],
    ))
    .unwrap();
    assert_eq!(page.entries[0].title, "筛选:标题关键词");

    // two filters at once -> the "多条件" branch, not a substring match.
    let page = pollster::block_on(engine.search_page(
        "",
        1,
        vec![
            FilterValue::Select {
                id: "genre".into(),
                value: "adv".into(),
            },
            FilterValue::Check {
                id: "completed".into(),
                value: 1,
            },
        ],
    ))
    .unwrap();
    assert_eq!(page.entries[0].title, "筛选:多条件");
}

#[test]
fn fixture_listings_return_distinct_pages() {
    let engine = load_fixture();
    let listings = pollster::block_on(engine.listings()).unwrap();
    assert_eq!(
        listings,
        vec![
            Listing {
                id: "popular".into(),
                name: "热门".into(),
                kind: ListingKind::Default
            },
            Listing {
                id: "latest".into(),
                name: "最新".into(),
                kind: ListingKind::Default
            }
        ]
    );

    let page1: MangaPageResult = pollster::block_on(engine.browse("popular", 1)).unwrap();
    assert_eq!(page1.entries[0].title, "热门-第1页");
    assert!(page1.has_next_page);

    let page2 = pollster::block_on(engine.browse("popular", 2)).unwrap();
    assert_eq!(page2.entries[0].title, "热门-第2页");
    assert!(!page2.has_next_page);

    let latest = pollster::block_on(engine.browse("latest", 1)).unwrap();
    assert_eq!(latest.entries[0].title, "最新-第1页");
}

#[test]
fn fixture_details_preserve_selected_identity_and_supply_two_numbered_chapters() {
    let engine = load_fixture();
    let listed = pollster::block_on(engine.browse("popular", 1)).unwrap();
    let selected = &listed.entries[0];
    let detail = pollster::block_on(engine.manga_details(&selected.key)).unwrap();
    assert_eq!(detail.source_key, "local.filter-acceptance");
    assert_eq!(detail.key, "fx-pop-1");
    assert_eq!(detail.title, "热门-第1页");
    assert!(detail.description.as_deref().unwrap().contains("本地日用验收"));
    let chapters = pollster::block_on(engine.chapters(&detail.key)).unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].key, "daily-2");
    assert_eq!(chapters[0].title.as_deref(), Some("第 2 话"));
    assert_eq!(chapters[0].chapter_number, Some(2.));
    assert_eq!(chapters[1].key, "daily-1");
    assert_eq!(chapters[1].title.as_deref(), Some("第 1 话"));
    assert_eq!(chapters[1].chapter_number, Some(1.));
}
