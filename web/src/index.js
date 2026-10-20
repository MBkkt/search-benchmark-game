import React from 'react';
import $ from 'jquery';
import ReactDOM from 'react-dom';
import './style.scss'
import * as serviceWorker from './serviceWorker';

function formatPercentVariation(p) {
  if (p !== undefined) {
    return "+" + (p * 100).toFixed(1) + " %";
  } else {
    return "";
  }
}

function formatBytes(n) {
  if (n === undefined || n === null) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  return (i === 0 ? v.toFixed(0) : v.toFixed(2)) + " " + units[i];
}

// Whole seconds as "45 s", "12 min" or "6.7 h": a build is read at a glance.
function formatDuration(secs) {
  if (secs < 120) return Math.round(secs) + " s";
  if (secs < 7200) return Math.round(secs / 60) + " min";
  return (secs / 3600).toFixed(1) + " h";
}

function numberWithCommas(x) {
  x = x.toString();
  let pattern = /(-?\d+)(\d{3})/
  while (pattern.test(x)) {
    x = x.replace(pattern, "$1,$2");
  }
  return x;
}

function stats(timings) {
  let median = timings[(timings.length / 2) | 0];
  let mean = timings.reduce(((pv, cv) => pv + cv), 0) / timings.length;
  return {
    "median": median,
    "mean": mean,
    "min": timings[0],
    "max": timings[timings.length - 1]
  };
}

function aggregate(query) {
  if (query.duration.length === 0) {
    return { query: query.query, className: "unsupported", unsupported: true }
  }
  var res = stats(query.duration);
  res.count = query.count;
  res.query = query.query;
  return res;
}

// Headline table: query shapes as rows, engines as columns. Each cell is that
// shape's AVERAGE latency (mean of per-query best timings) expressed relative to
// Lucene for the same shape (Lucene = 1.00, >1 = slower). The final "Mean" row
// carries two values per engine — the arithmetic mean of the per-shape ratios
// for the search (ranked top-k) shapes and, separately, for the count shapes —
// so a count strength/weakness never hides behind search latency or vice versa.
// Computed over ALL queries (no tag filter) so the headline is a fixed summary
// of the whole run.
function headlineSummary(data) {
  const modes = Object.keys(data.results);
  if (modes.length === 0) return null;
  const engines = Object.keys(data.results[modes[0]]);
  // The baseline column, in order of authority: named by the run, else the
  // lucene-named column (which is only correct by convention), else none.
  // A single-engine page has no baseline at all: a column normalised against
  // itself is 1.00 everywhere and says nothing, so such a page reports
  // absolute times instead.
  const declared = (data.run || {}).baseline;
  const luceneKey = engines.length < 2
    ? undefined
    : (engines.includes(declared) ? declared
       : engines.find(e => e.toLowerCase().indexOf("lucene") >= 0));

  // avg[engine][mode] = shape AVERAGE latency (undefined if any query unsupported).
  const avg = {};
  for (const engine of engines) {
    avg[engine] = {};
    for (const mode of modes) {
      const queries = data.results[mode][engine];
      if (!queries || queries.length === 0) { avg[engine][mode] = undefined; continue; }
      let total = 0;
      let unsupported = false;
      for (const q of queries) {
        if (!q.duration || q.duration.length === 0) { unsupported = true; break; }
        total += q.duration[0]; // best timing — the value the AVERAGE row sums
      }
      avg[engine][mode] = unsupported ? undefined : total / queries.length;
    }
  }

  // Split shapes into search (ranked top-k) and count families so the summary can
  // report them separately: a count command stresses posting-list traversal, a
  // top-k command stresses scoring, and one blended mean hides which an engine is
  // good at. Any shape whose name mentions COUNT (COUNT, TOP_*_COUNT) is a count
  // command; the rest are search.
  const isCount = m => m.toUpperCase().indexOf("COUNT") >= 0;
  const searchModes = modes.filter(m => !isCount(m));
  const countModes = modes.filter(m => isCount(m));

  // ratio[engine][mode] relative to Lucene. Each summary value is the arithmetic
  // mean of the per-shape ratios within one family — normalizing each shape to
  // Lucene first, then averaging, weights every shape equally so a single
  // high-latency shape can't dominate.
  const ratio = {};
  const summarySearch = {};
  const summaryCount = {};
  const meanOver = (engine, subset) => {
    let sum = 0, n = 0;
    for (const mode of subset) {
      const r = ratio[engine][mode];
      if (r !== undefined) { sum += r; n += 1; }
    }
    return n === 0 ? undefined : sum / n;
  };
  for (const engine of engines) {
    ratio[engine] = {};
    for (const mode of modes) {
      const base = luceneKey ? avg[luceneKey][mode] : undefined;
      const v = avg[engine][mode];
      ratio[engine][mode] = (v === undefined || !base) ? undefined : v / base;
    }
    summarySearch[engine] = meanOver(engine, searchModes);
    summaryCount[engine] = meanOver(engine, countModes);
  }
  return { engines, modes, searchModes, countModes, avg, ratio, summarySearch, summaryCount, luceneKey };
}

// Column order: every infino column together (the release, then main, then any
// fork branch spliced in as name@branch), Lucene next, everything else after in
// data order.
function orderEngines(engines) {
  const isInfino = e => e.toLowerCase().indexOf("infino") >= 0;
  const infino = engines.filter(e => isInfino(e) && e.indexOf("@") < 0)
    .concat(engines.filter(e => isInfino(e) && e.indexOf("@") >= 0));
  const lucene = engines.find(e => !isInfino(e) && e.toLowerCase().indexOf("lucene") >= 0);
  const head = lucene ? infino.concat([lucene]) : infino;
  return head.concat(engines.filter(e => head.indexOf(e) < 0));
}

// A column label a reader can scan: "lucene-10.5.0-bp" becomes "Lucene 10.5.0"
// over "+ doc reordering". Unknown names fall back to splitting off the version.
function engineLabel(name) {
  const m = /^([a-z]+)-?(.*)$/i.exec(name) || [name, name, ""];
  const pretty = { lucene: "Lucene", iresearch: "IResearch", infino: "infino", tantivy: "tantivy", turbopuffer: "turbopuffer" };
  const base = pretty[m[1].toLowerCase()] || m[1];
  let rest = m[2];
  let sub = "";
  let reordered = false;
  if (/-bp$/.test(rest)) { rest = rest.replace(/-bp$/, ""); reordered = true; }
  if (base === "infino" && !/^platform/.test(rest)) reordered = true;
  if (rest === "main") { rest = ""; sub = "main, unreleased"; }
  else if (base === "infino" && /^\d/.test(rest)) sub = "release";
  if (name.toLowerCase() === "turbopuffer") sub = "published numbers";
  return { title: (base + " " + rest).trim(), sub, reordered };
}

function EngineHeader({ name }) {
  const l = engineLabel(name);
  return <span className="eng-h" title={name}>{l.title}{l.sub ? <small>{l.sub}</small> : null}{l.reordered ? <small>+ doc reordering</small> : null}</span>;
}

// The engine with the lowest value in a row, or undefined when fewer than two
// engines have a value (a lone engine leads nothing).
function leaderOf(values) {
  const present = Object.keys(values).filter(e => values[e] !== undefined);
  if (present.length < 2) return undefined;
  return present.reduce((a, b) => (values[b] < values[a] ? b : a));
}

// Each engine's place in a row (1 = lowest value), plus the row's highest value
// for scaling bars. Equal values share a place.
function rowRanks(values) {
  const vs = Object.values(values).filter(v => v !== undefined).sort((a, b) => a - b);
  const rank = {};
  for (const e of Object.keys(values)) {
    if (values[e] !== undefined) rank[e] = vs.indexOf(values[e]) + 1;
  }
  return { rank, max: vs[vs.length - 1] };
}

function ordinal(n) {
  const s = n % 100 >= 11 && n % 100 <= 13 ? "th" : ({ 1: "st", 2: "nd", 3: "rd" }[n % 10] || "th");
  return n + s;
}

// Bar scale, relative to the baseline: 1.00 is a full bar at BAR_BASE% of the
// cell, and a value slower than the baseline continues past it in red, up to
// 1 + BAR_OVER_MAX, beyond which it stops at the cell edge (the value above it
// carries the exact figure). The 80% is repeated in style.scss's track.
const BAR_BASE = 80;
const BAR_OVER_MAX = 0.25;

function ratioBar(v) {
  const fill = Math.min(v, 1) * BAR_BASE;
  const over = Math.min(Math.max(v - 1, 0), BAR_OVER_MAX) / BAR_OVER_MAX * (100 - BAR_BASE);
  return (
    <div className="hl-bar">
      <span className="hl-fill" style={{ width: fill + "%" }} />
      {over > 0 ? <span className="hl-over" style={{ width: over + "%" }} /> : null}
    </div>
  );
}

// One ranked cell: the value, its bar (relative to the baseline; with no
// baseline, to the row's slowest engine), and the engine's place in the row.
// 1st is filled, 2nd and 3rd get a lighter shade of the same colour, the rest
// stay grey.
function rankedCell(v, text, place, max, absolute, key) {
  if (v === undefined) return <td className="hl-cell" key={key}>—</td>;
  const tier = place === 1 ? " hl-leader" : (place <= 3 ? " hl-p" + place : "");
  return (
    <td className={"hl-cell" + tier} key={key}>
      <div className="hl-val">{text}</div>
      {absolute
        ? <div className="hl-bar"><span className="hl-fill" style={{ width: (v / max) * BAR_BASE + "%" }} /></div>
        : ratioBar(v)}
      <div className="hl-gap">
        <b>{ordinal(place)}</b>
      </div>
    </td>
  );
}

// One Mean cell, two labeled values: the search top-k mean over the count mean.
function meanCell(searchR, countR, searchLead, countLead, key) {
  const num = (r, lead) => <span className={"hl-mean-num" + (lead ? " hl-mean-lead" : "")}>{r === undefined ? "—" : r.toFixed(2)}</span>;
  return (
    <td className="hl-cell hl-mean-cell" key={key}>
      <div className="hl-mean-item"><span className="hl-mean-lbl">search</span>{num(searchR, searchLead)}</div>
      <div className="hl-mean-item"><span className="hl-mean-lbl">count</span>{num(countR, countLead)}</div>
    </td>
  );
}

function Headline({ data }) {
  const summary = headlineSummary(data);
  if (!summary) return null;
  const { modes, ratio, avg, summarySearch, summaryCount, luceneKey } = summary;
  const engines = orderEngines(summary.engines);
  // With no baseline there is nothing to be relative to, so the table carries
  // the average latency per shape instead of a ratio.
  const absolute = luceneKey === undefined;
  const searchLead = leaderOf(summarySearch);
  const countLead = leaderOf(summaryCount);
  return (
    <div className="headline">
      <div className="headline-title">
        {absolute
          ? <>Average latency per query shape <span className="headline-hint">(no baseline column to normalise against, so these are absolute)</span></>
          : <>Latency relative to {luceneKey} = 1.00 <span className="headline-hint">(&gt;1 = slower; per shape, then mean split into search top-k and count)</span></>}
      </div>
      <table className="headline-table">
        <thead>
          <tr>
            <th>Query shape</th>
            {engines.map(engine => (
              <th key={"h-" + engine}><EngineHeader name={engine} /></th>
            ))}
          </tr>
        </thead>
        <tbody>
          {modes.map(mode => {
            const row = {};
            for (const e of engines) row[e] = absolute ? avg[e][mode] : ratio[e][mode];
            const ranks = rowRanks(row);
            return (
              <tr key={"r-" + mode}>
                <td className="hl-engine">{mode}</td>
                {engines.map(engine => {
                  const v = row[engine];
                  const text = v === undefined ? "" : (absolute ? numberWithCommas(Math.round(v)) + " µs" : v.toFixed(2));
                  return rankedCell(v, text, ranks.rank[engine], ranks.max, absolute, engine + "-" + mode);
                })}
              </tr>
            );
          })}
          {absolute ? null : (
            <tr className="hl-summary-row">
              <td className="hl-engine">Mean<div className="hl-mean-sublabel">search / count</div></td>
              {engines.map(engine => meanCell(summarySearch[engine], summaryCount[engine], engine === searchLead, engine === countLead, "s-" + engine))}
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

// On-disk index size per engine, sourced from data.index_sizes (a map
// engine -> bytes recorded at build time), with the smallest highlighted.
// Renders nothing for runs whose results.json predates index-size recording.
function IndexSize({ data }) {
  const sizes = data.index_sizes;
  if (!sizes || Object.keys(sizes).length === 0) return null;
  const present = Object.keys(sizes).filter(e => sizes[e] !== undefined && sizes[e] !== null);
  if (present.length === 0) return null;
  const engines = orderEngines(present);
  const luceneKey = engines.find(e => e.toLowerCase().indexOf("lucene") >= 0);
  const base = luceneKey ? sizes[luceneKey] : undefined;
  // Bars are relative to Lucene, as in the latency table; without a Lucene
  // column they fall back to the largest index.
  const row = Object.fromEntries(engines.map(e => [e, base ? sizes[e] / base : sizes[e]]));
  const ranks = rowRanks(row);
  return (
    <div className="headline">
      <div className="headline-title">
        Index size on disk{luceneKey ? <span className="headline-hint"> (and relative to Lucene = 1.00; &gt;1 = larger)</span> : null}
      </div>
      <table className="headline-table">
        <thead>
          <tr>
            <th>Metric</th>
            {engines.map(engine => <th key={"is-h-" + engine}><EngineHeader name={engine} /></th>)}
          </tr>
        </thead>
        <tbody>
          <tr>
            <td className="hl-engine">Size</td>
            {engines.map(engine => rankedCell(row[engine], formatBytes(sizes[engine]), ranks.rank[engine], ranks.max, !base, "is-s-" + engine))}
          </tr>
          {base ? (
            <tr>
              <td className="hl-engine">vs Lucene</td>
              {engines.map(engine => (
                <td className="hl-cell" key={"is-r-" + engine}>{(sizes[engine] / base).toFixed(2)}</td>
              ))}
            </tr>
          ) : null}
        </tbody>
      </table>
      <div className="headline-note">
        Note: infino’s on-disk index is columnar and stores the original{" "}
        <code>text</code> column for retrieval <em>in addition to</em>{" "}
        the full-text index.
        {engines.length > 1
          ? " The other engines here index the text without storing it, so their size is index-only."
          : null}
      </div>
    </div>
  );
}


// What produced this page: corpus, document count, query set, and when it ran.
// Renders nothing for runs whose results.json predates this block, which is
// every file committed before it existed.
//
// Styling rides the existing .headline / .details classes plus inline spacing,
// so the committed stylesheet needs no change and only the script bundle has
// to be rebuilt for this to appear.
function RunProvenance({ data }) {
  const run = data.run;
  if (!run || run.error) return null;
  const rows = [];
  if (run.corpus) {
    const c = run.corpus;
    const parts = [c.name];
    if (c.docs != null) parts.push(numberWithCommas(c.docs) + " documents");
    if (c.bytes != null) parts.push(formatBytes(c.bytes));
    rows.push(["Corpus", parts.join(", ")]);
  }
  if (run.queries && run.queries.name) {
    rows.push(["Queries", run.queries.count != null
      ? numberWithCommas(run.queries.count) + " (" + run.queries.name + ")"
      : run.queries.name]);
  }
  if (run.started_utc) {
    rows.push(["Run", run.started_utc.replace("T", " ").replace("+00:00", "") + " UTC"]);
  }
  // Only the engines built from a working tree carry a commit; everything
  // else here is pinned and has nothing to attribute.
  if (run.engines) {
    const built = Object.keys(run.engines).sort().map((name) => {
      const e = run.engines[name];
      return name + " at " + e.commit + (e.dirty ? " (uncommitted changes)" : "");
    });
    if (built.length) rows.push(["Engine build", built.join(", ")]);
  }
  // Settings that change what a timing means without changing any code.
  if (run.reader) {
    const r = run.reader;
    const parts = [];
    if (r.cache_budget_gb) {
      parts.push(r.cache_budget_gb === "unset"
        ? "disk cache budget unset, so the engine default applies"
        : "disk cache budget " + numberWithCommas(r.cache_budget_gb) + " GB");
    }
    if (r.query_threads) {
      parts.push(r.query_threads + (r.query_threads === "1" ? " query thread" : " query threads"));
    }
    if (parts.length) rows.push(["Reader", parts.join(", ")]);
  }
  // Present only when this run built the index it queried (a scale build).
  if (run.build) {
    const b = run.build;
    const parts = [];
    if (b.ingest_secs != null) parts.push("ingest " + formatDuration(b.ingest_secs));
    if (b.compact_secs != null) parts.push("compaction " + formatDuration(b.compact_secs));
    if (b.superfiles != null) parts.push(numberWithCommas(b.superfiles) + (b.superfiles === 1 ? " superfile" : " superfiles"));
    if (b.index_bytes != null) parts.push(formatBytes(b.index_bytes) + " in object storage");
    if (parts.length) rows.push(["Index build", parts.join(", ")]);
  }
  if (rows.length === 0) return null;
  return (
    <div className="headline" style={{ marginTop: "2rem", marginBottom: "2.5rem" }}>
      <div className="headline-title">What produced this page</div>
      <ul className="details">
        {rows.map(([label, value]) => (
          <li key={"run-" + label}><b>{label}:</b> {value}</li>
        ))}
      </ul>
    </div>
  );
}

function stats_row(engines, name, className, stat) {
  const lead = leaderOf(Object.fromEntries(Object.entries(engines).map(([e, s]) => [e, s && s[stat]])));
  return <tr className={className + "-row"}>
            <td>{name}</td>
            {
              Object.entries(engines).map(kv => {
                var engine = kv[0];
                var engine_stats = kv[1];
                if (engine_stats !== undefined) {
                  return <td key={"result-" + engine} className={engine === lead ? "stat-leader" : ""}>
                    {numberWithCommas(engine_stats[stat])} μs
                </td>;
                } else {
                  return <td key={"result-" + engine}>
                    Some Unsupported Queries
                </td>;
                }
              })
            }
          </tr>;
}

function details_list(engine_details) {
  return <ul className="details">
    {
      (engine_details || []).map(detail => {
        return <li>{detail}</li>;
      })
    }
    </ul>;
}

class Benchmark extends React.Component {

  constructor(props) {
    super(props);
    this.state = {
      mode: "TOP_10",
      tag: null
    };
  }

  handleChangeMode(evt) {
    this.setState({ mode: evt.target.value });
  }

  handleChangeTag(evt) {
    var tag = evt.target.value;
    if (tag === "ALL") {
      this.setState({ "tag": null });
    } else {
      this.setState({ "tag": tag });
    }
  }

  filterQueries(queries) {
    let tag = this.state.tag;
    if (tag !== null) {
      return queries.filter(query => query.tags.indexOf(tag) >= 0);
    } else {
      return queries;
    }
  }

  generateDataView() {
    var engines = {}
    var queries = {}
    var details = this.props.data.details;
    var mode_data = this.props.data.results[this.state.mode];
    for (var engine of orderEngines(Object.keys(mode_data))) {
      var engine_queries = mode_data[engine];
      engine_queries = Array.from(this.filterQueries(engine_queries));
      engine_queries = engine_queries.map(aggregate);
      var total = 0
      var p50 = 0
      var p90 = 0
      var p99 = 0
      var unsupported = false
      var all_latencies = []
      for (var query of engine_queries) {
        if (query.unsupported) {
          unsupported = true;
        } else {
          total += query.min;
          all_latencies.push(query.min);
        }
      }
      if (unsupported) {
        total = undefined;
        p50 = undefined;
        p90 = undefined;
        p99 = undefined;
      } else {
        total = (total / engine_queries.length) | 0;
        if (all_latencies.length !== 0) {
          all_latencies.sort(function(a, b) { return a - b; });
          p50 = all_latencies[Math.round((all_latencies.length - 1) * 0.5)];
          p90 = all_latencies[Math.round((all_latencies.length - 1) * 0.9)];
          p99 = all_latencies[Math.round((all_latencies.length - 1) * 0.99)];
        }
      }
      engines[engine] = unsupported ? undefined : { "average": total, "p50": p50, "p90": p90, "p99": p99 };
      for (let query of engine_queries) {
        var query_data = {};
        if (queries[query.query] !== undefined) {
          query_data = queries[query.query];
        }
        query_data[engine] = query
        queries[query.query] = query_data
      }
    }

    for (let query in queries) {
      let query_data = queries[query];
      var min_engine = null;
      var min_microsecs = 0;
      var max_engine = null;
      var max_microsecs = 0;
      for (let engine in query_data) {
        var engine_data = query_data[engine];
        if (engine_data.unsupported)
          continue;
        if (min_engine == null || engine_data.min < min_microsecs) {
          min_engine = engine;
          min_microsecs = engine_data.min;
        }
        if (max_engine == null || engine_data.min > max_microsecs) {
          max_engine = engine;
          max_microsecs = engine_data.min;
        }
      }
      for (let engine in query_data) {
        let engine_data = query_data[engine];
        if (engine_data.unsupported) continue;
        if (engine !== min_engine) {
          engine_data.variation = (engine_data.min - min_microsecs) / min_microsecs;
        }
      }
      // Only useful when two engines answered: a lone engine is both the
      // fastest and the slowest, and would be shaded as the slowest.
      if (min_engine != null && min_engine !== max_engine) {
        query_data[min_engine].className = "fastest";
        query_data[max_engine].className = "slowest";
      }
    }
    return { engines, queries, details };
  }

  render() {
    var data_view = this.generateDataView();
    return <div>
      <Headline data={this.props.data} />
      <IndexSize data={this.props.data} />
      <RunProvenance data={this.props.data} />
      <form>
        <fieldset>
          <label htmlFor="collectionField">Collection type</label>
          <select id="collectionField" onChange={evt => this.handleChangeMode(evt)}>
            {this.props.modes.map((mode) => <option value={mode} key={mode}>{mode}</option>)}
          </select>
          <label htmlFor="queryTagField">Type of Query</label>
          <select id="queryTagField" onChange={(evt) => this.handleChangeTag(evt)}>
            <option value="ALL" key="all">ALL</option>
            {this.props.tags.map((tag) => <option value={tag} key={tag}>{tag}</option>)}
          </select>
        </fieldset>
      </form>
      <table className="query-table">
        <thead>
          <tr>
            <th>Query</th>
            {
              Object.keys(data_view.engines).map((engine) => <th key={"col-" + engine}><details><summary><EngineHeader name={engine} /></summary>{ details_list(data_view.details[engine]) }</details></th>)
            }
          </tr>
        </thead>
        <tbody>
          { stats_row(data_view.engines, "AVERAGE", "average", "average") }
          { stats_row(data_view.engines, "P50", "percentile", "p50") }
          { stats_row(data_view.engines, "P90", "percentile", "p90") }
          { stats_row(data_view.engines, "P99", "percentile", "p99") }
          {
            Object.entries(data_view.queries).map(kv => {
              var query = kv[0];
              var engine_queries = kv[1];
              return <tr>
                <td>{query}</td>
                {
                  Object.keys(data_view.engines).map(engine => {
                    var cell_data = engine_queries[engine];
                    if (cell_data.unsupported) {
                      return <td className={"data " + cell_data.className}></td>;
                    } else {
                      return <td className={"data " + cell_data.className}>
                        <div className="timing">{numberWithCommas(cell_data.min)}  μs</div>
                        <div className="timing-variation">{formatPercentVariation(cell_data.variation)}</div>
                        <div className="count">{numberWithCommas(cell_data.count)} docs</div>
                      </td>;
                    }
                  })
                }
              </tr>
            })
          }
        </tbody>
      </table>
    </div>;
  }

}

$(function () {
  $.getJSON(process.env.PUBLIC_URL + "/results.json", (data) => {
    var modes = [];
    var engines = [];
    var tags_set = new Set();
    for (var mode in data.results) {
      modes.push(mode);
    }
    for (var engine in data.results[modes[0]]) {
      engines.push(engine);
    }
    for (var query of data.results[modes[0]][engines[0]]) {
      for (var tag of query.tags) {
        tags_set.add(tag);
      }
    }
    var tags = Array.from(tags_set);
    tags.sort();
    var el = document.getElementById("app-container");
    ReactDOM.render(<React.StrictMode>
      <Benchmark data={data} tags={tags} modes={modes} engines={engines} />
    </React.StrictMode>, el);
  });
});

// If you want your app to work offline and load faster, you can change
// unregister() to register() below. Note this comes with some pitfalls.
// Learn more about service workers: https://bit.ly/CRA-PWA
serviceWorker.unregister();
