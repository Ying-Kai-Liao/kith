#!/usr/bin/env node
// Writes each side's types for each contract (serve/contract/<name>/contract.json): the cloud HTTP API (TypeScript for
// the cloud and for desktop+web, Swift for iOS, Rust for the host and the guest) and the host RPC (TypeScript for
// desktop+web, Swift for iOS, Rust for the host). No dependencies: plain Node.
//
//   node serve/contract/gen.mjs           write the generated files
//   node serve/contract/gen.mjs --check   fail when one is missing or differs from what the contract makes

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");

/**
 * Each contract and the files made from it. `swiftNamespace` puts a contract's Swift types in an enum of their own
 * (`HostContract.Bot`), so they don't meet the app's own types of the same name in KithKit.
 */
const CONTRACTS = {
  cloud: {
    outputs: {
      "serve/cloud/src/contract.gen.ts": "ts",
      "apps/desktop/src/shared/cloud-contract.gen.ts": "ts",
      "apps/ios/Kit/Sources/KithKit/Models/CloudContract.gen.swift": "swift",
      "serve/host/crates/wire/src/cloud.rs": "rust",
    },
  },
  host: {
    swiftNamespace: "HostContract",
    outputs: {
      "apps/desktop/src/shared/host-contract.gen.ts": "ts",
      "apps/ios/Kit/Sources/KithKit/Models/HostContract.gen.swift": "swift",
      "serve/host/crates/wire/src/rpc.rs": "rust",
    },
  },
};

// The contract being written; set by `load`.
let NAME, C, sha, SWIFT_NS;

function load(name) {
  const raw = readFileSync(join(here, name, "contract.json"));
  NAME = name;
  sha = createHash("sha256").update(raw).digest("hex");
  C = JSON.parse(raw.toString("utf8"));
  SWIFT_NS = CONTRACTS[name].swiftNamespace ?? null;
  C.routes ??= {};
  C.methods ??= {};
  // A method's params are a type of their own, `<Method>Params`.
  for (const [m, def] of Object.entries(C.methods)) {
    const t = paramsType(m);
    if (C.types[t]) throw new Error(`${name}/contract.json: ${t} is both a type and method ${m}'s params`);
    C.types[t] = { doc: `What \`${m}\` takes.`, fields: def.params ?? {} };
  }
  validate();
}

// ---- the contract's shape ----

/** `json` is a value the contract leaves open (not typed yet): any JSON. */
const PRIMS = new Set(["string", "int", "bool", "json"]);
const RESERVED = "unknown";
/** Who may call a method: an invited person, or their device before it has shown its invite. */
const ROLES = new Set(["member", "unproven"]);

/** `[T]?` → { list: true, of: "T", opt: true }; `{T}` is a map of string keys to T. */
function parse(spec) {
  const opt = spec.endsWith("?");
  const s = opt ? spec.slice(0, -1) : spec;
  const list = s.startsWith("[") && s.endsWith("]");
  const map = s.startsWith("{") && s.endsWith("}");
  return { opt, list, map, of: list || map ? s.slice(1, -1) : s };
}

const paramsType = (method) => `${method[0].toUpperCase()}${method.slice(1)}Params`;

function validate() {
  const fail = (m) => {
    throw new Error(`${NAME}/contract.json: ${m}`);
  };
  for (const [name, e] of Object.entries(C.enums)) {
    if (!e.values?.length) fail(`enum ${name} has no values`);
    if (e.values.includes(RESERVED)) fail(`enum ${name}: "${RESERVED}" is a consumer's word for a value it doesn't know, never sent`);
    if (e.unknown !== RESERVED && !e.values.includes(e.unknown))
      fail(`enum ${name}: "unknown" says what a value a consumer doesn't know means: one of its values, or "${RESERVED}" for its own state`);
    if (C.types[name]) fail(`${name} is both an enum and a type`);
  }
  for (const [name, t] of Object.entries(C.types)) {
    for (const [f, spec] of Object.entries(t.fields)) {
      const { of } = parse(spec);
      if (!PRIMS.has(of) && !C.enums[of] && !C.types[of]) fail(`${name}.${f}: unknown type ${of}`);
    }
    for (const f of t.mayOmit ?? []) if (!parse(t.fields[f] ?? "x").opt) fail(`${name}.${f}: mayOmit needs an optional field`);
  }
  for (const [name, r] of Object.entries(C.routes)) {
    for (const k of ["request", "response"]) if (r[k] && !C.types[r[k]]) fail(`route ${name}: unknown ${k} ${r[k]}`);
    if ((r.kind ?? "json") === "json" && !r.response) fail(`route ${name}: a json route needs a response`);
  }
  for (const [name, m] of Object.entries(C.methods)) {
    if (!C.types[m.result]) fail(`method ${name}: unknown result ${m.result}`);
    if (!m.who?.length || m.who.some((w) => !ROLES.has(w))) fail(`method ${name}: who is some of ${[...ROLES]}`);
  }
}

const routeKind = (r) => r.kind ?? "json";
/** An enum whose unknown values are a state of their own (`unknown`), not read as one of its values. */
const isOpen = (e) => e.unknown === RESERVED;
const openEnums = () => Object.keys(C.enums).filter((n) => isOpen(C.enums[n]));
/** Each role and its methods, in the contract's order. */
const methodsOf = () => [...ROLES].map((r) => [r, Object.keys(C.methods).filter((m) => C.methods[m].who.includes(r))]).filter(([, ms]) => ms.length);
/** `v1/computers/{computerId}/grants` → [{ lit: "v1/computers/" }, { param: "computerId", any: false }, { lit: "/grants" }] */
function segments(path) {
  const out = [];
  let last = 0;
  for (const m of path.matchAll(/\{(\w+)(:any)?\}/g)) {
    if (m.index > last) out.push({ lit: path.slice(last, m.index) });
    out.push({ param: m[1], any: !!m[2] });
    last = m.index + m[0].length;
  }
  if (last < path.length) out.push({ lit: path.slice(last) });
  return out;
}
const params = (path) => segments(path).filter((s) => s.param);

const pascal = (s) => s[0].toUpperCase() + s.slice(1);
const snake = (s) => s.replace(/([a-z0-9])([A-Z])/g, "$1_$2").replace(/-/g, "_").toLowerCase();
const screaming = (s) => snake(s).toUpperCase();
// Rust keywords a field may be: a raw identifier (serde still uses the contract's name).
const RUST_KEYWORDS = new Set(
  ("as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut " +
    "pub ref return self static struct super trait true type unsafe use where while").split(" "),
);
const rsId = (n) => (RUST_KEYWORDS.has(n) ? `r#${n}` : n);
/** A name a language must use for an enum value (Swift's `none` reads as Optional.none). */
const caseName = (e, v) => e.names?.[v] ?? v.replace(/-(\w)/g, (_, c) => c.toUpperCase());

const header = (comment) =>
  [
    `${comment} Generated by serve/contract/gen.mjs from serve/contract/${NAME}/contract.json. Do not edit: change the`,
    `${comment} contract (additive only, serve/contract/README.md), then run \`node serve/contract/gen.mjs\`.`,
  ].join("\n");

const docLines = (doc, indent, mark) => (doc ? `${indent}${mark} ${doc}\n` : "");

// ---- TypeScript (the cloud, desktop and web) ----

function typescript() {
  const L = [];
  L.push(header("//"), "/* eslint-disable */", "", `export const CONTRACT_SHA256 = '${sha}'`, "");
  L.push("// ---- enums: a value a side doesn't know is decoded (decodeX) to what the contract says it means, never cast ----", "");
  for (const [name, e] of Object.entries(C.enums)) {
    L.push(docLines(e.doc, "", "/**").replace(/\n$/, " */\n").trimEnd());
    L.push(`export const ${name}Values = [${e.values.map((v) => `'${v}'`).join(", ")}] as const`);
    L.push(`export type ${name} = (typeof ${name}Values)[number]`);
    const out = isOpen(e) ? `${name} | 'unknown'` : name;
    const doc = isOpen(e) ? "one this version doesn't know is 'unknown'" : `one this version doesn't know is '${e.unknown}'`;
    L.push(`/** How a consumer reads a ${name} word: ${doc}. */`);
    L.push(`export const decode${name} = (raw: unknown): ${out} => known(${name}Values, raw, '${e.unknown}')`, "");
  }
  L.push("function known<T extends string, U extends string>(values: readonly T[], raw: unknown, otherwise: U): T | U {");
  L.push("  return (values as readonly unknown[]).includes(raw) ? (raw as T) : otherwise");
  L.push("}", "");
  L.push("// ---- types ----", "");
  const tsType = (spec) => {
    const { list, map, of } = parse(spec);
    const base = of === "int" ? "number" : of === "bool" ? "boolean" : of === "json" ? "unknown" : of;
    return list ? `${base}[]` : map ? `Record<string, ${base}>` : base;
  };
  for (const [name, t] of Object.entries(C.types)) {
    if (t.doc) L.push(`/** ${t.doc} */`);
    const fields = Object.entries(t.fields);
    if (!fields.length) {
      L.push(`export type ${name} = Record<string, never>`, "");
      continue;
    }
    L.push(`export interface ${name} {`);
    for (const [f, spec] of fields) {
      const { opt } = parse(spec);
      L.push(opt ? `  ${f}?: ${tsType(spec)} | null` : `  ${f}: ${tsType(spec)}`);
    }
    L.push("}", "");
  }
  L.push("export interface Types {");
  for (const name of Object.keys(C.types)) L.push(`  ${name}: ${name}`);
  L.push("}", "export type TypeName = keyof Types", "");
  L.push("type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false");
  L.push(`/** true for an enum whose unknown values are 'unknown': ${openEnums().join(", ") || "none"}. */`);
  L.push(`type IsOpen<T> = ${openEnums().map((n) => `Same<T, ${n}>`).join(" | ") || "false"}`);
  L.push("/**");
  L.push(" * A type as a consumer receives it (see `receive`): a field of an enum whose unknown values are their own state may");
  L.push(" * also be 'unknown'; any other enum's unknown value already reads as the value the contract names. A `json` value");
  L.push(" * (`unknown`) is received as it came.");
  L.push(" */");
  L.push("export type Received<T> = unknown extends T ? T : ReceivedValue<NonNullable<T>> | Extract<T, null | undefined>");
  L.push("type ReceivedValue<T> = true extends IsOpen<T>");
  L.push("  ? T | 'unknown'");
  L.push("  : T extends readonly (infer U)[]");
  L.push("    ? Received<U>[]");
  L.push("    : T extends object");
  L.push("      ? { [K in keyof T]: Received<T[K]> }");
  L.push("      : T", "");
  if (Object.keys(C.routes).length) tsRoutes(L);
  if (Object.keys(C.methods).length) tsMethods(L);
  L.push("// ---- checking and receiving ----", "");
  L.push("interface TypeSchema {");
  L.push("  fields: Record<string, string>");
  L.push("  mayOmit?: readonly string[]");
  L.push("}", "");
  L.push("interface EnumSchema {");
  L.push("  values: readonly string[]");
  L.push("  /** What a value this version doesn't know is read as. */");
  L.push("  unknown: string");
  L.push("}", "");
  L.push("export const SCHEMA: { enums: Record<string, EnumSchema>; types: Record<string, TypeSchema> } = {");
  L.push("  enums: {");
  for (const [name, e] of Object.entries(C.enums)) L.push(`    ${name}: { values: ${name}Values, unknown: '${e.unknown}' },`);
  L.push("  },", "  types: {");
  for (const [name, t] of Object.entries(C.types)) {
    const fields = Object.entries(t.fields).map(([f, s]) => `${f}: '${s}'`).join(", ");
    const body = fields ? `{ ${fields} }` : "{}";
    const omit = t.mayOmit ? `, mayOmit: [${t.mayOmit.map((f) => `'${f}'`).join(", ")}]` : "";
    L.push(`    ${name}: { fields: ${body}${omit} },`);
  }
  L.push("  },", "}", "");
  L.push(TS_RUNTIME);
  return L.join("\n").replace(/\n{3,}/g, "\n\n").trimEnd() + "\n";
}

function tsRoutes(L) {
  L.push("// ---- routes ----", "");
  L.push("export const ROUTES = {");
  for (const [name, r] of Object.entries(C.routes)) {
    const re = segments(r.path)
      .map((s) => (s.lit ? s.lit.replace(/[/.]/g, (c) => `\\${c}`) : s.any ? "([^/]+)" : "([A-Za-z0-9_-]{1,64})"))
      .join("");
    const extra = [r.request && `request: '${r.request}'`, r.response && `response: '${r.response}'`].filter(Boolean);
    L.push(
      `  ${name}: { method: '${r.method}', path: '${r.path}', pattern: /^\\/${re}$/, kind: '${routeKind(r)}'${extra.length ? ", " + extra.join(", ") : ""} },`,
    );
  }
  L.push("} as const", "export type RouteName = keyof typeof ROUTES", "");
  L.push("/** What each JSON route answers with. */");
  L.push("export interface Responses {");
  for (const [name, r] of Object.entries(C.routes)) if (r.response) L.push(`  ${name}: ${r.response}`);
  L.push("}", "", "/** What each route with a body takes. */", "export interface Requests {");
  for (const [name, r] of Object.entries(C.routes)) if (r.request) L.push(`  ${name}: ${r.request}`);
  L.push("}", "");
  L.push("/** Each route's path, without a leading slash; a route with a query has it (relay sockets: `?v=1`). */");
  L.push("export const paths = {");
  for (const [name, r] of Object.entries(C.routes)) {
    const ps = params(r.path);
    const body = segments(r.path).map((s) => (s.lit ?? `\${${s.param}}`)).join("") + (r.query ? `?${r.query}` : "");
    L.push(`  ${name}: (${ps.map((p) => `${p.param}: string`).join(", ")}) => \`${body}\`,`);
  }
  L.push("}", "");
  L.push(`/** ${C.errors.doc} */`);
  L.push("export const ERROR_STATUS = {");
  for (const [code, status] of Object.entries(C.errors.codes)) L.push(`  ${code}: ${status},`);
  L.push("} as const", "export type ErrorCode = keyof typeof ERROR_STATUS");
  L.push("export const isErrorCode = (code: unknown): code is ErrorCode => typeof code === 'string' && Object.hasOwn(ERROR_STATUS, code)", "");
}

function tsMethods(L) {
  L.push("// ---- methods ----", "");
  L.push("/** Each method: who may call it, what it takes and what it answers with. */");
  L.push("export const METHODS = {");
  for (const [name, m] of Object.entries(C.methods)) {
    L.push(`  ${name}: { who: [${m.who.map((w) => `'${w}'`).join(", ")}], params: '${paramsType(name)}', result: '${m.result}' },`);
  }
  L.push("} as const", "export type MethodName = keyof typeof METHODS", "");
  L.push("/** What each method takes. */", "export interface Params {");
  for (const name of Object.keys(C.methods)) L.push(`  ${name}: ${paramsType(name)}`);
  L.push("}", "", "/** What each method answers with. */", "export interface Results {");
  for (const [name, m] of Object.entries(C.methods)) L.push(`  ${name}: ${m.result}`);
  L.push("}", "");
  for (const [role, ms] of methodsOf()) {
    L.push(`/** What ${role === "member" ? "an invited person" : "an invited person's unproven device"} may call. */`);
    L.push(`export const ${screaming(role)}_METHODS = [${ms.map((m) => `'${m}'`).join(", ")}] as const satisfies readonly MethodName[]`, "");
  }
}


const TS_RUNTIME = `type Obj = Record<string, unknown>
const isObj = (v: unknown): v is Obj => typeof v === 'object' && v !== null && !Array.isArray(v)
const parseSpec = (spec: string) => {
  const opt = spec.endsWith('?')
  const s = opt ? spec.slice(0, -1) : spec
  const list = s.startsWith('[')
  const map = s.startsWith('{')
  return { opt, list, map, of: list || map ? s.slice(1, -1) : s }
}

/**
 * The problems with \`value\` as the contract type \`type\`; none is an empty list.
 * - 'producer' (what the cloud or the host must send): every field is there (null where it may be, absent only where
 *   mayOmit says), every enum value is a known one (never 'unknown'), and nothing the contract doesn't name is sent.
 * - 'consumer' (what an app must read): optional fields may be absent or null, unknown enum values and extra fields are
 *   fine. An older producer's answers (fixtures/compat/) and a newer one's (fixtures/unknown/) must pass.
 */
export function check(type: string, value: unknown, mode: 'producer' | 'consumer' = 'consumer', at: string = type): string[] {
  const t = SCHEMA.types[type]
  if (!t) return [\`\${at}: no type \${type} in the contract\`]
  if (!isObj(value)) return [\`\${at}: not an object\`]
  const out: string[] = []
  for (const [name, spec] of Object.entries(t.fields)) {
    const { opt } = parseSpec(spec)
    const v = value[name]
    if (v === undefined || v === null) {
      if (!opt) out.push(\`\${at}.\${name}: missing\`)
      else if (mode === 'producer' && v === undefined && !t.mayOmit?.includes(name)) out.push(\`\${at}.\${name}: missing (send null)\`)
      continue
    }
    out.push(...checkValue(spec, v, mode, \`\${at}.\${name}\`))
  }
  if (mode === 'producer') for (const k of Object.keys(value)) if (!(k in t.fields)) out.push(\`\${at}.\${k}: not in the contract\`)
  return out
}

function checkValue(spec: string, v: unknown, mode: 'producer' | 'consumer', at: string): string[] {
  const { list, map, of } = parseSpec(spec)
  if (list) return Array.isArray(v) ? v.flatMap((x, i) => checkValue(of, x, mode, \`\${at}[\${i}]\`)) : [\`\${at}: not a list\`]
  if (map) return isObj(v) ? Object.entries(v).flatMap(([k, x]) => checkValue(of, x, mode, \`\${at}.\${k}\`)) : [\`\${at}: not a map\`]
  if (of === 'json') return []
  if (of === 'string') return typeof v === 'string' ? [] : [\`\${at}: not a string\`]
  if (of === 'int') return Number.isSafeInteger(v) ? [] : [\`\${at}: not an integer\`]
  if (of === 'bool') return typeof v === 'boolean' ? [] : [\`\${at}: not a bool\`]
  const e = SCHEMA.enums[of]
  if (e) {
    if (typeof v !== 'string') return [\`\${at}: not a string\`]
    return mode === 'producer' && !e.values.includes(v) ? [\`\${at}: \${JSON.stringify(v)} is not a \${of}\`] : []
  }
  return check(of, v, mode, at)
}

/**
 * Reads an answer as a consumer: every enum value this version doesn't know becomes what the contract says it means
 * (its enum's \`unknown\`), and a list or map the answer left out is empty. Everything else, extra fields too, is passed
 * through as it came.
 */
export function receive<K extends TypeName>(type: K, raw: unknown): Received<Types[K]> {
  return receiveType(type, raw) as Received<Types[K]>
}

function receiveType(type: string, raw: unknown): Obj {
  const t = SCHEMA.types[type]
  if (!t || !isObj(raw)) return {}
  const out: Obj = { ...raw }
  for (const [name, spec] of Object.entries(t.fields)) {
    const { opt, list, map } = parseSpec(spec)
    const v = raw[name]
    if (v === undefined || v === null) {
      if (list && !opt) out[name] = []
      if (map && !opt) out[name] = {}
      continue
    }
    out[name] = receiveValue(spec, v)
  }
  return out
}

function receiveValue(spec: string, v: unknown): unknown {
  const { list, map, of } = parseSpec(spec)
  if (list) return Array.isArray(v) ? v.map((x) => receiveValue(of, x)) : v
  if (map) return isObj(v) ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, receiveValue(of, x)])) : v
  const e = SCHEMA.enums[of]
  if (e) return e.values.includes(v as string) ? v : e.unknown
  if (SCHEMA.types[of]) return isObj(v) ? receiveType(of, v) : v
  return v
}`;

// ---- Swift (iOS) ----

// Swift keywords a contract word may be: declared in backticks (a member reference after `.` needs none).
const SWIFT_KEYWORDS = new Set(
  ("associatedtype class deinit enum extension fileprivate func import init inout internal let open operator private " +
    "protocol public rethrows static struct subscript typealias var break case catch continue default defer do else " +
    "fallthrough for guard if in repeat return throw switch where while Any as false is nil self Self super throws true try")
    .split(" "),
);
const swId = (n) => (SWIFT_KEYWORDS.has(n) ? `\`${n}\`` : n);

function swift() {
  const top = SWIFT_NS ?? `${pascal(NAME)}Contract`;
  const L = [];
  L.push(header("//"), "", "import Foundation", "");
  L.push(`/// The ${NAME} contract these types were made from.`);
  L.push(`public enum ${top} {`, `    public static let sha256 = "${sha}"`, "}", "");
  // B: what goes in the namespace (or at the top level when the contract has none).
  const B = [];
  for (const [name, e] of Object.entries(C.enums)) {
    B.push(docLines(e.doc, "", "///").trimEnd());
    B.push(`public enum ${name}: String, Codable, Hashable, Sendable {`);
    for (const v of e.values) {
      const n = caseName(e, v);
      B.push(n === v ? `    case ${swId(n)}` : `    case ${swId(n)} = "${v}"`);
    }
    if (isOpen(e)) B.push("    /// A value this version doesn't know.", "    case unknown");
    B.push("");
    B.push(`    /// A value this version doesn't know is \`.${isOpen(e) ? "unknown" : caseName(e, e.unknown)}\`.`);
    B.push("    public init(from decoder: Decoder) throws {");
    B.push(`        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .${isOpen(e) ? "unknown" : caseName(e, e.unknown)}`);
    B.push("    }", "}", "");
  }
  const usesJson = Object.values(C.types).some((t) => Object.values(t.fields).some((spec) => parse(spec).of === "json"));
  if (usesJson) B.push(SWIFT_JSON_VALUE, "");
  const swType = (spec) => {
    const { list, map, of, opt } = parse(spec);
    const base = of === "string" ? "String" : of === "int" ? "Int64" : of === "bool" ? "Bool" : of === "json" ? "JSONValue" : of;
    return (list ? `[${base}]` : map ? `[String: ${base}]` : base) + (opt ? "?" : "");
  };
  for (const [name, t] of Object.entries(C.types)) {
    if (t.doc) B.push(`/// ${t.doc}`);
    const fields = Object.entries(t.fields);
    B.push(`public struct ${name}: Codable, Hashable, Sendable {`);
    for (const [f, spec] of fields) B.push(`    public var ${swId(f)}: ${swType(spec)}`);
    if (fields.length) B.push("");
    const args = fields.map(([f, spec]) => `${swId(f)}: ${swType(spec)}${parse(spec).opt ? " = nil" : ""}`).join(", ");
    B.push(`    public init(${args}) {`);
    for (const [f] of fields) B.push(`        self.${swId(f)} = ${swId(f)}`);
    B.push("    }", "}", "");
  }
  if (Object.keys(C.methods).length) {
    B.push("/// Each method's name, as called.");
    B.push("public enum Method: String, CaseIterable, Sendable {");
    for (const name of Object.keys(C.methods)) B.push(`    case ${swId(name)}`);
    B.push("}", "");
    for (const [role, ms] of methodsOf()) {
      B.push(`/// What ${role === "member" ? "an invited person" : "an invited person's unproven device"} may call.`);
      B.push(`public static let ${role}Methods: [Method] = [${ms.map((m) => `.${m}`).join(", ")}]`, "");
    }
  }
  if (Object.keys(C.routes).length) {
    B.push("/// Each route's path, without a leading slash; a route with a query has it (relay sockets: `?v=1`).");
    B.push("public enum CloudPath {");
    for (const [name, r] of Object.entries(C.routes)) {
      const ps = params(r.path);
      const body = segments(r.path).map((s) => (s.lit ?? `\\(${s.param})`)).join("") + (r.query ? `?${r.query}` : "");
      if (!ps.length) B.push(`    public static let ${name} = "${body}"`);
      else B.push(`    public static func ${name}(${ps.map((p) => `_ ${p.param}: String`).join(", ")}) -> String { "${body}" }`);
    }
    B.push("}", "");
  }
  if (C.errors) {
    B.push(`/// ${C.errors.doc}`);
    B.push("public enum CloudErrorCode: String, Sendable {");
    for (const code of Object.keys(C.errors.codes)) B.push(`    case ${swId(code)}`);
    B.push("    /// A code this version doesn't know.", "    case unknown", "");
    B.push("    public init(code: String) {", "        self = Self(rawValue: code) ?? .unknown", "    }", "");
    B.push("    public var status: Int? {", "        switch self {");
    for (const [code, status] of Object.entries(C.errors.codes)) B.push(`        case .${code}: ${status}`);
    B.push("        case .unknown: nil", "        }", "    }", "}", "");
  }
  const D = [];
  D.push("/// Decodes `data` as the contract type `name`; false when the contract has no such type. For the fixtures' tests.");
  D.push("public static func decode(_ name: String, _ data: Data) throws -> Bool {");
  D.push("    let d = JSONDecoder()", "    switch name {");
  for (const name of Object.keys(C.types)) D.push(`    case "${name}": _ = try d.decode(${name}.self, from: data)`);
  D.push("    default: return false", "    }", "    return true", "}");
  const indent = (lines) => lines.join("\n").split("\n").map((l) => (l ? `    ${l}` : l));
  if (SWIFT_NS) L.push(`extension ${top} {`, ...indent([...B, ...D]), "}");
  else L.push(...B, `extension ${top} {`, ...indent(D), "}");
  return L.join("\n").replace(/\n{3,}/g, "\n\n").trimEnd() + "\n";
}

/** Any JSON: a value the contract leaves open. Decoded as it came, so a newer producer's value is kept. */
const SWIFT_JSON_VALUE = `/// Any JSON: a value the contract leaves open (not typed yet). Kept as it came.
public enum JSONValue: Codable, Hashable, Sendable {
    case null
    case bool(Bool)
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let v = try? c.decode(Bool.self) {
            self = .bool(v)
        } else if let v = try? c.decode(Int64.self) {
            self = .int(v)
        } else if let v = try? c.decode(Double.self) {
            self = .double(v)
        } else if let v = try? c.decode(String.self) {
            self = .string(v)
        } else if let v = try? c.decode([JSONValue].self) {
            self = .array(v)
        } else {
            self = .object(try c.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case let .bool(v): try c.encode(v)
        case let .int(v): try c.encode(v)
        case let .double(v): try c.encode(v)
        case let .string(v): try c.encode(v)
        case let .array(v): try c.encode(v)
        case let .object(v): try c.encode(v)
        }
    }
}`;

// ---- Rust (the host and the guest) ----

function rust() {
  const L = [];
  L.push(header("//!"));
  if (Object.keys(C.routes).length) L.push("//!", "//! Paths here start with a slash, as the host and the guest sign them.");
  L.push("");
  L.push(
    "#![allow(clippy::doc_markdown, clippy::struct_field_names, clippy::struct_excessive_bools, reason = \"generated from the contract\")]",
    "",
  );
  const usesMap = Object.values(C.types).some((t) => Object.values(t.fields).some((spec) => parse(spec).map));
  if (usesMap) L.push("use std::collections::BTreeMap;", "");
  L.push("use serde::{Deserialize, Serialize};", "");
  L.push("/// The sha256 of the contract these types were made from.", `pub const CONTRACT_SHA256: &str = "${sha}";`, "");
  for (const [name, e] of Object.entries(C.enums)) {
    L.push(docLines(e.doc, "", "///").trimEnd());
    L.push("#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]");
    L.push(`pub enum ${name} {`);
    // serde reads a word it doesn't know as the variant marked `other`, which must be the last.
    for (const v of [...e.values.filter((x) => x !== e.unknown), ...e.values.filter((x) => x === e.unknown)]) {
      if (v === e.unknown) L.push("    /// Also what a value this version doesn't know is read as.", "    #[default]");
      L.push(v === e.unknown ? `    #[serde(rename = "${v}", other)]` : `    #[serde(rename = "${v}")]`, `    ${pascal(caseName(e, v))},`);
    }
    if (isOpen(e)) L.push("    /// A value this version doesn't know.", "    #[default]", '    #[serde(rename = "unknown", other)]', "    Unknown,");
    L.push("}", "");
    L.push(`impl ${name} {`, "    pub const fn as_str(self) -> &'static str {", "        match self {");
    for (const v of e.values) L.push(`            Self::${pascal(caseName(e, v))} => "${v}",`);
    if (isOpen(e)) L.push('            Self::Unknown => "unknown",');
    L.push("        }", "    }", "}", "");
  }
  const rsType = (spec) => {
    const { list, map, of, opt } = parse(spec);
    const base = of === "string" ? "String" : of === "int" ? "i64" : of === "bool" ? "bool" : of === "json" ? "serde_json::Value" : of;
    const t = list ? `Vec<${base}>` : map ? `BTreeMap<String, ${base}>` : base;
    return opt ? `Option<${t}>` : t;
  };
  for (const [name, t] of Object.entries(C.types)) {
    if (t.doc) L.push(`/// ${t.doc}`);
    const fields = Object.entries(t.fields);
    L.push("#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]");
    if (fields.length) L.push('#[serde(rename_all = "camelCase")]');
    if (!fields.length) {
      L.push(`pub struct ${name} {}`, "");
      continue;
    }
    L.push(`pub struct ${name} {`);
    for (const [f, spec] of fields) {
      if (parse(spec).opt) L.push('    #[serde(default, skip_serializing_if = "Option::is_none")]');
      L.push(`    pub ${rsId(snake(f))}: ${rsType(spec)},`);
    }
    L.push("}", "");
  }
  if (Object.keys(C.routes).length) {
    L.push("/// Each route's path and query, as signed.", "pub mod path {");
    for (const [name, r] of Object.entries(C.routes)) {
      const ps = params(r.path);
      const body = "/" + segments(r.path).map((s) => (s.lit ?? `{${snake(s.param)}}`)).join("") + (r.query ? `?${r.query}` : "");
      if (!ps.length) L.push(`    pub const ${screaming(name)}: &str = "${body}";`);
      else {
        L.push(`    pub fn ${snake(name)}(${ps.map((p) => `${snake(p.param)}: &str`).join(", ")}) -> String {`);
        L.push(`        format!("${body}")`, "    }");
      }
    }
    L.push("}", "");
  }
  if (C.errors) {
    L.push(`/// ${C.errors.doc}`, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]", "pub enum ErrorCode {");
    for (const code of Object.keys(C.errors.codes)) L.push(`    ${pascal(code)},`);
    L.push("    /// A code this version doesn't know.", "    Unknown,", "}", "");
    L.push("impl ErrorCode {", "    pub fn from_code(code: &str) -> Self {", "        match code {");
    for (const code of Object.keys(C.errors.codes)) L.push(`            "${code}" => Self::${pascal(code)},`);
    L.push("            _ => Self::Unknown,", "        }", "    }", "");
    L.push("    pub const fn as_str(self) -> &'static str {", "        match self {");
    for (const code of Object.keys(C.errors.codes)) L.push(`            Self::${pascal(code)} => "${code}",`);
    L.push('            Self::Unknown => "unknown",', "        }", "    }", "");
    L.push("    pub const fn status(self) -> Option<u16> {", "        match self {");
    const byStatus = new Map();
    for (const [code, status] of Object.entries(C.errors.codes)) byStatus.set(status, [...(byStatus.get(status) ?? []), code]);
    for (const [status, codes] of byStatus) L.push(`            ${codes.map((c) => `Self::${pascal(c)}`).join(" | ")} => Some(${status}),`);
    L.push("            Self::Unknown => None,", "        }", "    }", "}", "");
  }
  if (Object.keys(C.methods).length) rsMethods(L);
  L.push("/// Decodes `value` as the contract type `name`; None when the contract has no such type. For the fixtures' tests.");
  L.push("pub fn decode(name: &str, value: serde_json::Value) -> Option<Result<(), serde_json::Error>> {");
  L.push("    Some(match name {");
  for (const name of Object.keys(C.types)) L.push(`        "${name}" => serde_json::from_value::<${name}>(value).map(drop),`);
  L.push("        _ => return None,", "    })", "}");
  return L.join("\n").replace(/\n{3,}/g, "\n\n").trimEnd() + "\n";
}

function rsMethods(L) {
  L.push("/// Each method's name, as called.", "pub mod method {");
  for (const [name, m] of Object.entries(C.methods)) {
    if (m.doc) L.push(`    /// ${m.doc}`);
    L.push(`    pub const ${screaming(name)}: &str = "${name}";`);
  }
  L.push("}", "");
  for (const [role, ms] of methodsOf()) {
    L.push(`/// What ${role === "member" ? "an invited person" : "an invited person's unproven device"} may call.`);
    L.push(`pub const ${screaming(role)}_METHODS: &[&str] = &[${ms.map((m) => `method::${screaming(m)}`).join(", ")}];`, "");
  }
  L.push("/// The contract as data, for `crate::check` (the producer's strict check and the consumer's).");
  L.push("pub static SCHEMA: crate::check::Schema = crate::check::Schema {");
  L.push("    enums: &[");
  for (const [name, e] of Object.entries(C.enums)) {
    L.push(`        crate::check::EnumSchema { name: "${name}", values: &[${e.values.map((v) => `"${v}"`).join(", ")}], unknown: "${e.unknown}" },`);
  }
  L.push("    ],", "    types: &[");
  for (const [name, t] of Object.entries(C.types)) {
    const fields = Object.entries(t.fields).map(([f, spec]) => `("${f}", "${spec}")`).join(", ");
    const omit = (t.mayOmit ?? []).map((f) => `"${f}"`).join(", ");
    L.push(`        crate::check::TypeSchema { name: "${name}", fields: &[${fields}], may_omit: &[${omit}] },`);
  }
  L.push("    ],", "    methods: &[");
  for (const [name, m] of Object.entries(C.methods)) {
    L.push(
      `        crate::check::MethodSchema { name: "${name}", who: &[${m.who.map((w) => `"${w}"`).join(", ")}], params: "${paramsType(name)}", result: "${m.result}" },`,
    );
  }
  L.push("    ],", "};", "");
}

// ---- write or check ----

const checking = process.argv.includes("--check");
let stale = 0;
for (const name of Object.keys(CONTRACTS)) {
  load(name);
  const made = { ts: typescript(), swift: swift(), rust: rust() };
  for (const [path, kind] of Object.entries(CONTRACTS[name].outputs)) {
    const text = made[kind];
    const file = join(root, path);
    let now = null;
    try {
      now = readFileSync(file, "utf8");
    } catch {}
    if (now === text) continue;
    if (checking) {
      console.error(`${relative(process.cwd(), file)} is not what serve/contract/${name}/contract.json makes: run node serve/contract/gen.mjs`);
      stale++;
    } else {
      writeFileSync(file, text);
      console.log(`wrote ${path}`);
    }
  }
}
if (stale) process.exit(1);
