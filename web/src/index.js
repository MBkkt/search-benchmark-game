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

// Column order: infino first, Lucene second, everything else after in data order.
function orderEngines(engines) {
  const lucene = engines.find(e => e.toLowerCase().indexOf("lucene") >= 0);
  const infino = engines.find(e => e.toLowerCase().indexOf("infino") >= 0 && e.indexOf("@") < 0)
    || engines.find(e => e.toLowerCase().indexOf("infino") >= 0);
  const head = [];
  if (infino) head.push(infino);
  if (lucene && lucene !== infino) head.push(lucene);
  return head.concat(engines.filter(e => e !== infino && e !== lucene));
}

function ratioCell(r, isBase, extraCls, key) {
  let cls = "hl-cell" + (extraCls ? " " + extraCls : "");
  if (isBase) cls += " hl-base";
  else if (r !== undefined) cls += r > 1.005 ? " hl-slower" : (r < 0.995 ? " hl-faster" : "");
  return <td className={cls} key={key}>{r === undefined ? "—" : r.toFixed(2)}</td>;
}

function meanNumClass(r, isBase) {
  if (isBase || r === undefined) return "hl-mean-num";
  return "hl-mean-num " + (r > 1.005 ? "hl-mean-slower" : (r < 0.995 ? "hl-mean-faster" : ""));
}

// One Mean cell, two labeled values: the search top-k mean over the count mean.
function meanCell(searchR, countR, isBase, key) {
  const num = (r) => <span className={meanNumClass(r, isBase)}>{r === undefined ? "—" : r.toFixed(2)}</span>;
  return (
    <td className={"hl-cell hl-mean-cell" + (isBase ? " hl-base" : "")} key={key}>
      <div className="hl-mean-item"><span className="hl-mean-lbl">search</span>{num(searchR)}</div>
      <div className="hl-mean-item"><span className="hl-mean-lbl">count</span>{num(countR)}</div>
    </td>
  );
}

// Microseconds, as the absolute-time cells render them.
function usCell(v, key) {
  return <td key={key} className="hl-num">{v === undefined ? "—" : numberWithCommas(Math.round(v)) + " \u00b5s"}</td>;
}

function Headline({ data }) {
  const summary = headlineSummary(data);
  if (!summary) return null;
  const { modes, ratio, avg, summarySearch, summaryCount, luceneKey } = summary;
  const engines = orderEngines(summary.engines);
  // With no baseline there is nothing to be relative to, so the table carries
  // the average latency per shape instead of a ratio.
  const absolute = luceneKey === undefined;
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
              <th key={"h-" + engine}>{engine}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {modes.map(mode => (
            <tr key={"r-" + mode}>
              <td className="hl-engine">{mode}</td>
              {engines.map(engine => absolute
                ? usCell(avg[engine][mode], engine + "-" + mode)
                : ratioCell(ratio[engine][mode], engine === luceneKey, "", engine + "-" + mode))}
            </tr>
          ))}
          {absolute ? null : (
            <tr className="hl-summary-row">
              <td className="hl-engine">Mean<div className="hl-mean-sublabel">search / count</div></td>
              {engines.map(engine => meanCell(summarySearch[engine], summaryCount[engine], engine === luceneKey, "s-" + engine))}
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

// On-disk index size per engine, sourced from data.index_sizes (a map
// engine -> bytes recorded at build time). Engines are columns in the same
// order as the headline; two rows — absolute size and size relative to
// Lucene (Lucene = 1.00, >1 = larger). Renders nothing for runs whose
// results.json predates index-size recording.
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

function IndexSize({ data }) {
  const sizes = data.index_sizes;
  if (!sizes || Object.keys(sizes).length === 0) return null;
  const present = Object.keys(sizes).filter(e => sizes[e] !== undefined && sizes[e] !== null);
  if (present.length === 0) return null;
  const engines = orderEngines(present);
  const luceneKey = engines.find(e => e.toLowerCase().indexOf("lucene") >= 0);
  const base = luceneKey ? sizes[luceneKey] : undefined;
  const ratioClass = (r) => r > 1.005 ? "hl-slower" : (r < 0.995 ? "hl-faster" : "");
  return (
    <div className="headline">
      <div className="headline-title">
        Index size on disk{luceneKey ? <span className="headline-hint"> (and relative to Lucene = 1.00; &gt;1 = larger)</span> : null}
      </div>
      <table className="headline-table">
        <thead>
          <tr>
            <th>Metric</th>
            {engines.map(engine => <th key={"is-h-" + engine}>{engine}</th>)}
          </tr>
        </thead>
        <tbody>
          <tr>
            <td className="hl-engine">Size</td>
            {engines.map(engine => (
              <td className={"hl-cell" + (engine === luceneKey ? " hl-base" : "")} key={"is-s-" + engine}>
                {formatBytes(sizes[engine])}
              </td>
            ))}
          </tr>
          {base ? (
            <tr>
              <td className="hl-engine">vs Lucene</td>
              {engines.map(engine => {
                const r = sizes[engine] / base;
                const isBase = engine === luceneKey;
                let cls = "hl-cell";
                if (isBase) cls += " hl-base";
                else cls += " " + ratioClass(r);
                return <td className={cls} key={"is-r-" + engine}>{r.toFixed(2)}</td>;
              })}
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

function stats_row(engines, name, className, stat) {
  return <tr className={className + "-row"}>
            <td>{name}</td>
            {
              Object.entries(engines).map(kv => {
                var engine = kv[0];
                var engine_stats = kv[1];
                if (engine_stats !== undefined) {
                  return <td key={"result-" + engine}>
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
    for (var engine in mode_data) {
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
      if (min_engine != null) {
        // Only useful if at least one engine supports this query 
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
      <hr />
      <table>
        <thead>
          <tr>
            <th>Query</th>
            {
              Object.keys(data_view.engines).map((engine) => <th key={"col-" + engine}><details><summary>{engine}</summary>{ details_list(data_view.details[engine]) }</details></th>)
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
