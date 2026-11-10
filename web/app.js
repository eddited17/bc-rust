// bc-rust web client: renders Business Central pages from AL page metadata.
"use strict";
const $ = (s, el = document) => el.querySelector(s);
const main = $("#main");
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const metaCache = new Map();

async function api(path, opts = {}) {
  const t0 = performance.now();
  const r = await fetch(path, { headers: { "content-type": "application/json" }, ...opts });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  const ms = performance.now() - t0;
  if (!r.ok) throw Object.assign(new Error(j.error || r.statusText), { status: r.status });
  status(j, ms);
  return j;
}

function status(j, ms) {
  const parts = [`round trip ${ms.toFixed(1)} ms`];
  if (j && j.ms !== undefined) parts.push(`server ${j.ms.toFixed(2)} ms`);
  if (j && j.steps) parts.push(`${j.steps} AL statements`);
  if (j && j.scanned !== undefined) parts.push(`${j.scanned} rows scanned`);
  $("#status").innerHTML = parts.map((p) => `<span>${esc(p)}</span>`).join("") + `<span style="margin-left:auto">AL interpreter · embedded store · no SQL Server</span>`;
  if (j && j.messages && j.messages.length) showDialog("Message", j.messages.join("\n\n"));
}

function showDialog(title, body, isError) {
  $("#dialog-title").textContent = title;
  $("#dialog-title").className = "dialog-title" + (isError ? " err-title" : "");
  $("#dialog-body").textContent = body;
  $("#dialog").classList.remove("hidden");
  $("#dialog-ok").focus();
}
$("#dialog-ok").onclick = () => $("#dialog").classList.add("hidden");
document.addEventListener("keydown", (e) => { if (e.key === "Escape") $("#dialog").classList.add("hidden"); });

async function meta(id) {
  if (!metaCache.has(id)) metaCache.set(id, await api(`/api/page/${id}`));
  return metaCache.get(id);
}

// ---------------------------------------------------------------- Tell me
const tm = $("#tellme"), tmr = $("#tellme-results");
let tmTimer;
tm.addEventListener("input", () => {
  clearTimeout(tmTimer);
  tmTimer = setTimeout(async () => {
    const q = tm.value.trim();
    if (!q) return tmr.classList.add("hidden");
    const hits = await fetch(`/api/pages?q=${encodeURIComponent(q)}`).then((r) => r.json());
    tmr.innerHTML = hits.map((h, i) => `<a href="#/page/${h.id}" class="${i === 0 ? "sel" : ""}"><span>${esc(h.caption)}</span><small>${esc(h.type)} · ${h.id}</small></a>`).join("") || `<a><span>No results</span></a>`;
    tmr.classList.remove("hidden");
  }, 120);
});
tm.addEventListener("keydown", (e) => {
  if (e.key === "Enter") { const a = tmr.querySelector("a.sel"); if (a && a.getAttribute("href")) location.hash = a.getAttribute("href"); tmr.classList.add("hidden"); tm.blur(); }
});
document.addEventListener("click", (e) => { if (!e.target.closest(".tellme")) tmr.classList.add("hidden"); });
tmr.addEventListener("click", () => { tmr.classList.add("hidden"); tm.value = ""; });

// ---------------------------------------------------------------- routing
window.addEventListener("hashchange", route);
async function route() {
  const h = location.hash.slice(1) || "/";
  const [path, qs] = h.split("?");
  const q = new URLSearchParams(qs || "");
  try {
    const m = path.match(/^\/page\/(\d+)$/);
    if (m) {
      const md = await meta(+m[1]);
      if (isListType(md.type)) return renderList(md, q);
      return renderCard(md, q);
    }
    return renderHome();
  } catch (e) {
    main.innerHTML = `<h1>Something went wrong</h1><p>${esc(e.message)}</p>`;
  }
}
const isListType = (t) => ["list", "listpart", "worksheet", "listplus"].includes(String(t).toLowerCase());

async function renderHome() {
  const [home, st] = await Promise.all([api("/api/home"), fetch("/api/stats").then((r) => r.json())]);
  $("#company").textContent = home.company;
  main.innerHTML = `
    <h1>Role Center</h1>
    <div class="sub">${st.files.toLocaleString()} AL files · ${st.lines.toLocaleString()} lines · ${st.tables} tables · ${st.pages} pages · ${st.codeunits} codeunits · ${st.subscribers} event subscribers — parsed in ${st.parseMs} ms, server booted in ${st.bootMs} ms · ${st.storeRows.toLocaleString()} rows in ${st.storeTables} tables</div>
    <div class="groups">${home.groups.map((g) => `
      <div class="group"><h2>${esc(g.group)}</h2><div class="tiles">${g.pages.map((p) => `
        <a class="tile" href="#/page/${p.id}"><span class="n">${p.count}</span><span class="c">${esc(p.caption)}</span></a>`).join("")}</div></div>`).join("")}
    </div>`;
}

// ---------------------------------------------------------------- list pages
function repeaterFields(md) {
  const out = [];
  const walk = (cs, inRep) => cs.forEach((c) => {
    if (c.kind === "field" && inRep) out.push(c);
    if (c.children) walk(c.children, inRep || c.kind === "repeater");
  });
  walk(md.layout, false);
  if (!out.length) walk(md.layout, true);
  return out.filter((f) => f.field);
}
const isNum = (f) => /Decimal|Integer|BigInteger/.test(f.field?.type || "");

async function renderList(md, q, into, link, onChange) {
  const target = into || main;
  const cols = repeaterFields(md).filter((c) => c.importance !== "Additional").slice(0, 14);
  if (!into) {
    target.innerHTML = `
      <h1>${esc(md.caption)}<span class="pill">${esc(md.type)} ${md.id}</span></h1>
      ${md.about ? `<div class="about">${esc(md.about)}</div>` : ""}
      <div class="toolbar">
        ${md.insertAllowed && md.cardPage ? `<button class="primary" id="new">+ New</button>` : ""}
        <input class="search" id="search" placeholder="Search" value="${esc(q?.get("search") || "")}">
        <button id="refresh">Refresh</button>
        ${actionButtons(md, true)}
      </div>
      <div id="grid"></div>`;
    $("#new")?.addEventListener("click", () => (location.hash = `#/page/${md.cardPage}?new=1`));
    $("#refresh").onclick = () => load();
    let t;
    $("#search").addEventListener("input", () => { clearTimeout(t); t = setTimeout(load, 200); });
  }
  const gridEl = into ? target : $("#grid");
  const editablePart = !!into && md.editable && String(md.type).toLowerCase() === "listpart";
  async function load() {
    const search = into ? "" : $("#search").value;
    const params = new URLSearchParams({ top: 500 });
    if (search) params.set("search", search);
    if (link) params.set("link", JSON.stringify(link));
    const data = await api(`/api/page/${md.id}/rows?${params}`);
    const cell = (c, r) => {
      const v = r[c.field.name];
      if (editablePart && c.editable) {
        const f = c.field;
        if (f.type === "Boolean") return `<td><input type="checkbox" data-f="${esc(f.name)}" ${v ? "checked" : ""}></td>`;
        if (f.options && f.options.length)
          return `<td><select data-f="${esc(f.name)}">${f.options.map((o) => `<option ${o === v ? "selected" : ""}>${esc(o)}</option>`).join("")}</select></td>`;
        return `<td><input data-f="${esc(f.name)}" class="${isNum(c) ? "num" : ""}" value="${esc(v ?? "")}"></td>`;
      }
      const s = v === true ? "✓" : v === false ? "" : v;
      return `<td class="${isNum(c) ? "num" : ""}">${esc(s)}</td>`;
    };
    gridEl.innerHTML = `${editablePart ? `<div class="toolbar"><button class="primary" data-act="newline">+ New line</button><button data-act="delline">Delete line</button></div>` : ""}
      <table class="grid ${editablePart ? "edit" : ""}"><thead><tr>${cols.map((c) => `<th class="${isNum(c) ? "num" : ""}">${esc(c.caption)}</th>`).join("")}</tr></thead>
      <tbody>${data.rows.map((r, i) => `<tr data-i="${i}">${cols.map((c) => cell(c, r)).join("")}</tr>`).join("")}</tbody></table>
      ${data.rows.length ? "" : `<p class="sub">No rows to show.</p>`}
      ${data.more ? `<p class="sub">Showing the first ${data.rows.length} rows.</p>` : ""}`;
    let selected = null;
    gridEl.querySelectorAll("tbody tr").forEach((tr) => {
      tr.onclick = () => {
        const row = data.rows[+tr.dataset.i];
        gridEl.querySelectorAll("tr.sel").forEach((x) => x.classList.remove("sel"));
        tr.classList.add("sel");
        selected = row;
        if (!editablePart && md.cardPage) location.hash = `#/page/${md.cardPage}?key=${encodeURIComponent(JSON.stringify(row.$key))}`;
      };
    });
    if (!editablePart) return;
    gridEl.onchange = async (e) => {
      const el = e.target.closest("[data-f]");
      if (!el) return;
      const row = data.rows[+el.closest("tr").dataset.i];
      try {
        await api(`/api/page/${md.id}/record?key=${encodeURIComponent(JSON.stringify(row.$key))}`, { method: "PATCH", body: JSON.stringify({ [el.dataset.f]: el.type === "checkbox" ? el.checked : el.value }) });
      } catch (err) { showDialog("bc-rust", err.message, true); }
      await load();
      onChange && onChange();
    };
    $('[data-act="newline"]', gridEl).onclick = async () => {
      try { await api(`/api/page/${md.id}/record`, { method: "POST", body: JSON.stringify(link || {}) }); } catch (err) { showDialog("bc-rust", err.message, true); }
      await load();
    };
    $('[data-act="delline"]', gridEl).onclick = async () => {
      if (!selected) return showDialog("bc-rust", "Select a line first.");
      try { await api(`/api/page/${md.id}/record?key=${encodeURIComponent(JSON.stringify(selected.$key))}`, { method: "DELETE" }); } catch (err) { showDialog("bc-rust", err.message, true); }
      await load();
      onChange && onChange();
    };
  }
  await load();
}

// ---------------------------------------------------------------- actions
const PREFERRED = ["post", "release", "reopen", "postandsend", "preview", "calculateinvoicediscount", "copydocument"];
function actionButtons(md, listMode) {
  const runnable = md.actions.filter((a) => a.runnable && !listMode);
  runnable.sort((a, b) => {
    const ia = PREFERRED.indexOf(a.name.toLowerCase()), ib = PREFERRED.indexOf(b.name.toLowerCase());
    return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
  });
  const shown = runnable.slice(0, 6).map((a) => `<button data-action="${esc(a.name)}" title="${esc(a.tooltip || "")}">${esc(a.caption)}</button>`);
  const rest = md.actions.filter((a) => !a.runnable || listMode).slice(0, listMode ? 6 : 3).map((a) => `<button disabled title="${esc(a.tooltip || "")}">${esc(a.caption)}</button>`);
  return shown.concat(rest).join("");
}

// ---------------------------------------------------------------- card pages
function inputFor(c, value, editable) {
  const f = c.field;
  if (!f) return `<input readonly value="${esc(value ?? "")}">`;
  const ro = editable ? "" : "readonly";
  if (f.type === "Boolean") return `<input type="checkbox" data-f="${esc(f.name)}" ${value ? "checked" : ""} ${editable ? "" : "disabled"}>`;
  if (f.options && f.options.length && editable)
    return `<select data-f="${esc(f.name)}">${f.options.map((o) => `<option ${o === value ? "selected" : ""}>${esc(o)}</option>`).join("")}</select>`;
  return `<input data-f="${esc(f.name)}" class="${isNum(c) ? "num" : ""}" value="${esc(value ?? "")}" ${ro} title="${esc(c.tooltip || "")}">`;
}

async function renderCard(md, q) {
  let key = q.get("key") ? JSON.parse(q.get("key")) : null;
  let rec = null;
  if (q.get("new")) {
    const res = await api(`/api/page/${md.id}/record`, { method: "POST", body: "{}" });
    rec = res.record;
    key = rec.$key;
    history.replaceState(null, "", `#/page/${md.id}?key=${encodeURIComponent(JSON.stringify(key))}`);
  } else {
    rec = await api(`/api/page/${md.id}/record` + (key ? `?key=${encodeURIComponent(JSON.stringify(key))}` : ""));
    key = rec.$key;
  }
  const pkNames = (md.table && md.table.pk) || [];
  const title = () => {
    const keyParts = pkNames.length ? pkNames.map((n) => rec[n]).filter((v) => v && v !== md.caption.split(" ").pop()) : rec.$key || [];
    const name = rec.Name || rec.Description || rec["Sell-to Customer Name"] || rec["Buy-from Vendor Name"];
    return [md.caption, ...keyParts].concat(name ? [name] : []).filter(Boolean).join(" · ");
  };
  const content = md.layout.find((a) => a.kind === "area") ? md.layout.filter((a) => a.kind === "area" && a.name.toLowerCase() === "content").flatMap((a) => a.children) : md.layout;
  main.innerHTML = `
    <h1 id="title">${esc(title())}<span class="pill">${esc(md.type)} ${md.id}</span></h1>
    <div class="toolbar">
      ${md.insertAllowed ? `<button id="new">+ New</button>` : ""}
      ${md.deleteAllowed ? `<button id="del">Delete</button>` : ""}
      ${actionButtons(md)}
    </div>
    <div id="tabs"></div>`;
  const tabs = $("#tabs");
  const editableCard = md.editable;
  content.forEach((g, gi) => {
    if (g.kind === "part") {
      const div = document.createElement("div");
      div.className = "fasttab part";
      div.innerHTML = `<div class="hd"><span>${esc(g.caption)}</span></div><div class="bd"></div>`;
      tabs.appendChild(div);
      meta(g.page).then((pm) => {
        const link = {};
        g.link.forEach((l) => { link[l.field] = l.kind === "Field" ? rec[l.value] ?? "" : l.value; });
        renderList(pm, null, $(".bd", div), link, async () => {
          rec = await api(`/api/page/${md.id}/record?key=${encodeURIComponent(JSON.stringify(key))}`);
          tabs.querySelectorAll("[data-f]").forEach((el) => { const v = rec[el.dataset.f]; if (el.type === "checkbox") el.checked = !!v; else el.value = v ?? ""; });
        });
      });
      return;
    }
    const fields = [];
    const walk = (cs) => cs.forEach((c) => { if (c.kind === "field") fields.push(c); else if (c.children && c.kind !== "part") walk(c.children); });
    walk(g.kind === "field" ? [g] : g.children || []);
    if (!fields.length) return;
    const div = document.createElement("div");
    div.className = "fasttab" + (gi > 2 ? " closed" : "");
    const hasMore = fields.some((f) => f.importance === "Additional");
    const peekVals = () => fields.filter((f) => f.importance === "Promoted" && f.field).map((f) => rec[f.field.name]).filter((v) => v !== "" && v !== false && v != null).join(" · ");
    div.innerHTML = `<div class="hd"><span>${esc(g.caption)}</span><span class="peek">${esc(peekVals())}</span></div>
      <div class="bd"><div class="fields">${fields.map((c) => {
        const v = c.field ? rec[c.field.name] : "";
        const ed = editableCard && c.editable && !(c.field && c.field.pk && rec.$key && rec.$key.some((k) => k));
        return `<div class="fld ${c.importance === "Additional" ? "additional" : ""}"><label title="${esc(c.tooltip || "")}">${esc(c.caption)}</label>${inputFor(c, v, ed)}</div>`;
      }).join("")}</div>${hasMore ? `<a href="javascript:void 0" class="more">Show more</a>` : ""}</div>`;
    $(".hd", div).onclick = () => div.classList.toggle("closed");
    const more = $(".more", div);
    if (more) more.onclick = () => { div.classList.toggle("show-more"); more.textContent = div.classList.contains("show-more") ? "Show less" : "Show more"; };
    tabs.appendChild(div);
  });

  async function save(fname, value) {
    try {
      const res = await api(`/api/page/${md.id}/record?key=${encodeURIComponent(JSON.stringify(key))}`, { method: "PATCH", body: JSON.stringify({ [fname]: value }) });
      rec = res.record;
      key = rec.$key;
    } catch (e) {
      showDialog("bc-rust", e.message, true);
    }
    // refresh every field: validation triggers often change other fields
    tabs.querySelectorAll("[data-f]").forEach((el) => {
      const v = rec[el.dataset.f];
      if (el.type === "checkbox") el.checked = !!v; else if (document.activeElement !== el || el.dataset.f === fname) el.value = v ?? "";
    });
    $("#title").firstChild.textContent = title();
  }
  tabs.addEventListener("change", (e) => {
    const el = e.target.closest("[data-f]");
    if (!el) return;
    save(el.dataset.f, el.type === "checkbox" ? el.checked : el.value);
  });
  main.querySelectorAll("[data-action]").forEach((b) => {
    b.onclick = async () => {
      b.disabled = true;
      try {
        const res = await api(`/api/page/${md.id}/action/${encodeURIComponent(b.dataset.action)}?key=${encodeURIComponent(JSON.stringify(key))}`, { method: "POST" });
        if (res.deleted) {
          showDialog("bc-rust", `${b.textContent} completed. ${md.caption} ${key.filter(Boolean).join(" ")} was processed and is no longer open.`);
          history.back();
          return;
        }
        route();
      } catch (e) { showDialog("bc-rust", e.message, true); }
      b.disabled = false;
    };
  });
  $("#new")?.addEventListener("click", () => (location.hash = `#/page/${md.id}?new=1`));
  $("#del")?.addEventListener("click", async () => {
    if (!confirm(`Delete ${title()}?`)) return;
    try {
      await api(`/api/page/${md.id}/record?key=${encodeURIComponent(JSON.stringify(key))}`, { method: "DELETE" });
      history.back();
    } catch (e) { showDialog("bc-rust", e.message, true); }
  });
}

fetch("/api/home").then((r) => r.json()).then((h) => ($("#company").textContent = h.company));
route();
