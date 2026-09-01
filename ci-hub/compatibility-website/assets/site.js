"use strict";

(function (root) {
  const QUERY_ORDER = [
    "q",
    "lane",
    "category",
    "mode",
    "backend",
    "state",
    "result",
    "selected_by_full",
    "selected_by_path",
    "current_main_ancestry",
    "host_capability",
    "host_capability_present",
    "evidence",
    "population",
    "sort",
    "direction",
  ];
  const DIRECT_FILTERS = ["lane", "category", "mode", "backend", "state", "result", "evidence"];
  const CURRENT_ONLY = [
    ...DIRECT_FILTERS,
    "selected_by_full",
    "selected_by_path",
    "current_main_ancestry",
    "host_capability",
    "host_capability_present",
  ];
  const ENUMS = {
    state: ["green", "red", "never-measured", "unavailable"],
    result: ["pass", "product_failure", "unavailable"],
    selected_by_full: ["true", "false"],
    selected_by_path: ["quick", "full", "super"],
    current_main_ancestry: ["true", "false", "unavailable"],
    host_capability_present: ["true", "false", "unavailable"],
    evidence: ["complete", "incomplete"],
    population: ["in_manifest", "not_in_current_manifest"],
    sort: [
      "manifest",
      "cell",
      "test",
      "mode",
      "backend",
      "state",
      "result",
      "latest_result_time",
      "latest_result_tree",
      "physical_row_count",
      "represented_run_count",
    ],
    direction: ["asc", "desc"],
  };
  const OUTSIDE_SORTS = new Set(["cell", "physical_row_count", "represented_run_count"]);
  const NUMERIC_SORTS = new Set(["manifest", "physical_row_count", "represented_run_count"]);
  const STATE_ORDER = new Map([
    ["green", 0],
    ["red", 1],
    ["never-measured", 2],
    ["unavailable", 3],
  ]);

  function defaults(population) {
    const value = population || "in_manifest";
    return {
      q: "",
      lane: "",
      category: "",
      mode: "",
      backend: "",
      state: "",
      result: "",
      selected_by_full: "",
      selected_by_path: "",
      current_main_ancestry: "",
      host_capability: "",
      host_capability_present: "",
      evidence: "",
      population: value,
      sort: value === "in_manifest" ? "manifest" : "cell",
      direction: "asc",
    };
  }

  function invalid(message) {
    return { ok: false, error: message };
  }

  function allowedValue(name, value, allowed) {
    const fixed = ENUMS[name];
    if (fixed) {
      return fixed.includes(value);
    }
    const values = allowed[name];
    return Array.isArray(values) && values.includes(value);
  }

  function parseQuery(search, allowed) {
    const raw = search.startsWith("?") ? search.slice(1) : search;
    if (/%(?![0-9a-f]{2})/i.test(raw)) {
      return invalid("the URL contains an invalid percent escape");
    }
    const params = new URLSearchParams(raw);
    const supplied = {};
    for (const [name, value] of params) {
      if (!QUERY_ORDER.includes(name)) {
        return invalid(`the URL contains unsupported filter ${name}`);
      }
      if (Object.prototype.hasOwnProperty.call(supplied, name)) {
        return invalid(`the URL repeats filter ${name}`);
      }
      supplied[name] = value;
    }
    const population = supplied.population || "in_manifest";
    if (!allowedValue("population", population, allowed)) {
      return invalid(`population has unsupported value ${population}`);
    }
    const state = defaults(population);
    for (const name of QUERY_ORDER) {
      if (!Object.prototype.hasOwnProperty.call(supplied, name)) {
        continue;
      }
      const value = supplied[name];
      if (name === "q") {
        const trimmed = value.trim();
        if (!trimmed || trimmed.length > 256) {
          return invalid("q must contain between 1 and 256 non-space characters");
        }
        state.q = trimmed;
        continue;
      }
      if (!value || !allowedValue(name, value, allowed)) {
        return invalid(`${name} has unsupported value ${value || "(empty)"}`);
      }
      state[name] = value;
    }
    const capability = Boolean(state.host_capability);
    const capabilityValue = Boolean(state.host_capability_present);
    if (capability !== capabilityValue) {
      return invalid("host_capability and host_capability_present must be supplied together");
    }
    if (population === "not_in_current_manifest") {
      const unsupported = CURRENT_ONLY.find((name) => state[name]);
      if (unsupported) {
        return invalid(`${unsupported} is unavailable outside the current manifest`);
      }
      if (!OUTSIDE_SORTS.has(state.sort)) {
        return invalid(`${state.sort} cannot sort identities outside the current manifest`);
      }
    }
    return { ok: true, state };
  }

  function serializeState(state) {
    const params = new URLSearchParams();
    const baseline = defaults(state.population);
    for (const name of QUERY_ORDER) {
      const value = state[name];
      if (name === "population") {
        if (value !== "in_manifest") {
          params.append(name, value);
        }
        continue;
      }
      if (value && value !== baseline[name]) {
        params.append(name, value);
      }
    }
    return params.toString();
  }

  function unavailableRowEvidenceMatches(state) {
    if (state.current_main_ancestry && state.current_main_ancestry !== "unavailable") {
      return false;
    }
    if (state.host_capability && state.host_capability_present !== "unavailable") {
      return false;
    }
    return true;
  }

  function oneEvidenceRowMatches(evidence, state) {
    if (
      state.current_main_ancestry &&
      evidence.current_main_ancestry !== state.current_main_ancestry
    ) {
      return false;
    }
    if (state.host_capability) {
      const capabilities = evidence.host_capabilities || {};
      const value = Object.prototype.hasOwnProperty.call(capabilities, state.host_capability)
        ? capabilities[state.host_capability]
        : "unavailable";
      if (value !== state.host_capability_present) {
        return false;
      }
    }
    return true;
  }

  function rowScopedEvidenceMatches(record, state) {
    if (!state.current_main_ancestry && !state.host_capability) {
      return true;
    }
    if (record.series_evidence.length === 0) {
      return unavailableRowEvidenceMatches(state);
    }
    return record.series_evidence.some((evidence) => oneEvidenceRowMatches(evidence, state));
  }

  function matches(record, state) {
    if (record.population !== state.population) {
      return false;
    }
    if (state.q && !record.search.toLowerCase().includes(state.q.toLowerCase())) {
      return false;
    }
    if (state.population === "not_in_current_manifest") {
      return true;
    }
    for (const name of DIRECT_FILTERS) {
      if (state[name] && record[name] !== state[name]) {
        return false;
      }
    }
    if (state.selected_by_full && record.selected_by_full !== state.selected_by_full) {
      return false;
    }
    if (state.selected_by_path && !record.selected_by_paths.includes(state.selected_by_path)) {
      return false;
    }
    return rowScopedEvidenceMatches(record, state);
  }

  function stringCompare(left, right) {
    if (left === right) {
      return 0;
    }
    return left < right ? -1 : 1;
  }

  function comparePrimary(left, right, sort) {
    if (sort === "state") {
      return STATE_ORDER.get(left[sort]) - STATE_ORDER.get(right[sort]);
    }
    if (NUMERIC_SORTS.has(sort)) {
      return Number(left[sort]) - Number(right[sort]);
    }
    const leftValue = left[sort] || "";
    const rightValue = right[sort] || "";
    if (!leftValue || !rightValue) {
      if (!leftValue && !rightValue) {
        return 0;
      }
      return leftValue ? -1 : 1;
    }
    return stringCompare(leftValue, rightValue);
  }

  function sortRecords(records, state) {
    return records.slice().sort((left, right) => {
      const leftUnavailable = !NUMERIC_SORTS.has(state.sort) && !left[state.sort];
      const rightUnavailable = !NUMERIC_SORTS.has(state.sort) && !right[state.sort];
      if (leftUnavailable !== rightUnavailable) {
        return leftUnavailable ? 1 : -1;
      }
      const primary = comparePrimary(left, right, state.sort);
      if (primary !== 0) {
        return state.direction === "desc" ? -primary : primary;
      }
      return stringCompare(left.cell, right.cell);
    });
  }

  function summarize(records) {
    return records.reduce(
      (summary, record) => ({
        matching: summary.matching + 1,
        physical: summary.physical + record.physical_row_count,
        represented: summary.represented + record.represented_run_count,
      }),
      { matching: 0, physical: 0, represented: 0 },
    );
  }

  const api = Object.freeze({ matches, parseQuery, serializeState, sortRecords, summarize });
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
  root.HermitCompatibilityCellListing = api;

  function parseJsonAttribute(row, name) {
    const value = row.dataset[name];
    if (!value) {
      throw new Error(`rendered row is missing ${name}`);
    }
    return JSON.parse(value);
  }

  function integerAttribute(row, name) {
    const value = Number(row.dataset[name]);
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new Error(`rendered row has invalid ${name}`);
    }
    return value;
  }

  function manifestRecord(row, index) {
    return {
      element: row,
      population: "in_manifest",
      manifest: index,
      cell: row.dataset.cell,
      test: row.dataset.test,
      lane: row.dataset.lane,
      category: row.dataset.category,
      mode: row.dataset.mode,
      backend: row.dataset.backend,
      state: row.dataset.state,
      result: row.dataset.result,
      selected_by_full: row.dataset.selectedByFull,
      selected_by_paths: parseJsonAttribute(row, "selectedByPaths"),
      evidence: row.dataset.evidence,
      latest_result_time: row.dataset.latestResultTime,
      latest_result_tree: row.dataset.latestResultTree,
      search: row.dataset.search,
      series_evidence: parseJsonAttribute(row, "seriesEvidence"),
      physical_row_count: integerAttribute(row, "physicalRowCount"),
      represented_run_count: integerAttribute(row, "representedRunCount"),
    };
  }

  function outsideRecord(row, index) {
    return {
      element: row,
      population: "not_in_current_manifest",
      manifest: index,
      cell: row.dataset.cell,
      search: row.dataset.search,
      series_evidence: [],
      physical_row_count: integerAttribute(row, "physicalRowCount"),
      represented_run_count: integerAttribute(row, "representedRunCount"),
    };
  }

  function selectValues(form, name) {
    const control = form.elements.namedItem(name);
    return Array.from(control.options)
      .map((option) => option.value)
      .filter(Boolean);
  }

  function allowedValues(form) {
    return {
      lane: selectValues(form, "lane"),
      category: selectValues(form, "category"),
      mode: selectValues(form, "mode"),
      backend: selectValues(form, "backend"),
      host_capability: selectValues(form, "host_capability"),
    };
  }

  function setControls(form, state) {
    for (const name of QUERY_ORDER) {
      const control = form.elements.namedItem(name);
      if (control) {
        control.value = state[name];
      }
    }
  }

  function stateFromControls(form) {
    const state = {};
    for (const name of QUERY_ORDER) {
      const control = form.elements.namedItem(name);
      state[name] = control ? control.value.trim() : "";
    }
    return state;
  }

  function setCurrentManifestControls(form, population) {
    const outside = population === "not_in_current_manifest";
    for (const name of CURRENT_ONLY) {
      const control = form.elements.namedItem(name);
      control.disabled = outside;
      if (outside) {
        control.value = "";
      }
    }
    const sort = form.elements.namedItem("sort");
    for (const option of sort.options) {
      option.disabled = outside && !OUTSIDE_SORTS.has(option.value);
    }
    if (outside && !OUTSIDE_SORTS.has(sort.value)) {
      sort.value = "cell";
    }
    if (!outside) {
      const capability = form.elements.namedItem("host_capability");
      form.elements.namedItem("host_capability_present").disabled = !capability.value;
    }
  }

  function updateUrl(state) {
    const url = new URL(root.location.href);
    url.search = serializeState(state);
    url.hash = state.population === "in_manifest" ? "all-cells" : "recorded-not-in-current-manifest";
    root.history.replaceState(null, "", url.href);
  }

  function browserView(form) {
    const currentRows = Array.from(document.querySelectorAll("[data-cell-row='1']")).map(
      manifestRecord,
    );
    const outsideRows = Array.from(
      document.querySelectorAll("[data-recorded-not-in-manifest-row='1']"),
    ).map(outsideRecord);
    return {
      form,
      currentRows,
      outsideRows,
      allRecords: currentRows.concat(outsideRows),
      currentBody: document.getElementById("cell-results"),
      outsideBody: document.getElementById("recorded-not-in-current-manifest-results"),
      currentSections: Array.from(document.querySelectorAll("[data-in-manifest-listing]")),
      outsideSection: document.getElementById("recorded-not-in-current-manifest"),
      error: document.getElementById("cell-filter-error"),
      empty: document.getElementById("cell-filter-empty"),
      matchingCount: document.querySelector("[data-matching-count]"),
      counts: document.getElementById("cell-filter-counts"),
      physicalCount: document.querySelector("[data-physical-count]"),
      representedCount: document.querySelector("[data-represented-count]"),
      matchingLabel: document.querySelector("[data-matching-label]"),
      evidenceLabel: document.querySelector("[data-evidence-label]"),
      allowed: allowedValues(form),
    };
  }

  function showInvalid(view, message) {
    view.form.reset();
    setCurrentManifestControls(view.form, "in_manifest");
    view.error.textContent = `Filters were not applied: ${message}.`;
    view.error.hidden = false;
    view.empty.hidden = true;
    view.counts.hidden = true;
    for (const record of sortRecords(view.currentRows, defaults("in_manifest"))) {
      record.element.hidden = false;
      view.currentBody.appendChild(record.element);
    }
    for (const record of sortRecords(view.outsideRows, defaults("not_in_current_manifest"))) {
      record.element.hidden = false;
      view.outsideBody.appendChild(record.element);
    }
    for (const section of view.currentSections) {
      section.hidden = false;
    }
    view.outsideSection.hidden = false;
  }

  function showFormError(view, message) {
    view.error.textContent = `Filters were not applied: ${message}.`;
    view.error.hidden = false;
  }

  function applyView(view, state, replaceUrl) {
    const populationRecords = view.allRecords.filter(
      (record) => record.population === state.population,
    );
    const matching = sortRecords(
      populationRecords.filter((record) => matches(record, state)),
      state,
    );
    const matchingElements = new Set(matching.map((record) => record.element));
    for (const record of populationRecords) {
      record.element.hidden = !matchingElements.has(record.element);
    }
    const body = state.population === "in_manifest" ? view.currentBody : view.outsideBody;
    for (const record of matching) {
      body.appendChild(record.element);
    }
    const current = state.population === "in_manifest";
    for (const section of view.currentSections) {
      section.hidden = !current;
    }
    view.outsideSection.hidden = current;
    const summary = summarize(matching);
    view.matchingCount.textContent = String(summary.matching);
    view.physicalCount.textContent = String(summary.physical);
    view.representedCount.textContent = String(summary.represented);
    view.matchingLabel.textContent = current
      ? "matching cells in the manifest"
      : "matching recorded cell identities not in the current manifest";
    view.evidenceLabel.textContent = current ? "those cells have" : "those identities have";
    view.empty.hidden = summary.matching !== 0;
    view.counts.hidden = false;
    view.error.hidden = true;
    if (replaceUrl) {
      updateUrl(state);
    }
  }

  function applyControls(view) {
    const state = stateFromControls(view.form);
    const result = parseQuery(`?${serializeState(state)}`, view.allowed);
    if (!result.ok) {
      showFormError(view, result.error);
      return;
    }
    applyView(view, result.state, true);
  }

  function applyLocation(view) {
    const result = parseQuery(root.location.search, view.allowed);
    if (!result.ok) {
      showInvalid(view, result.error);
      return;
    }
    setControls(view.form, result.state);
    setCurrentManifestControls(view.form, result.state.population);
    applyView(view, result.state, false);
  }

  function bind(view) {
    const form = view.form;
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      applyControls(view);
    });
    form.addEventListener("input", (event) => {
      if (event.target.name === "q") {
        applyControls(view);
      }
    });
    form.addEventListener("change", (event) => {
      if (event.target.name === "population") {
        setCurrentManifestControls(form, event.target.value);
      } else if (event.target.name === "host_capability") {
        const present = form.elements.namedItem("host_capability_present");
        present.disabled = !event.target.value;
        if (!event.target.value) {
          present.value = "";
        }
      }
      applyControls(view);
    });
    form.querySelector("[data-clear-cell-filters]").addEventListener("click", () => {
      form.reset();
      setCurrentManifestControls(form, "in_manifest");
      applyView(view, defaults("in_manifest"), true);
    });
    root.addEventListener("popstate", () => applyLocation(view));
  }

  function initialize() {
    const form = document.querySelector("[data-cell-filter-form]");
    if (!form) {
      return;
    }
    form.hidden = false;
    try {
      const view = browserView(form);
      bind(view);
      applyLocation(view);
    } catch (errorValue) {
      const error = document.getElementById("cell-filter-error");
      error.textContent = `Filters were not applied: ${
        errorValue instanceof Error ? errorValue.message : String(errorValue)
      }.`;
      error.hidden = false;
    }
  }

  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", initialize, { once: true });
    } else {
      initialize();
    }
  }
})(globalThis);
