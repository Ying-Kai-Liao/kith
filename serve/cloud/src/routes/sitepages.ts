// The site's inner pages: each is static content in routes/site/<name>.ts, drawn in the shared frame (routes/site.ts).

import type { Ctx } from "../api";
import { sitePage } from "./site";
import { PAGE as agents } from "./site/agents";
import { PAGE as docs } from "./site/docs";
import { PAGE as privacy } from "./site/privacy";
import { PAGE as security } from "./site/security";
import { PAGE as sharing } from "./site/sharing";

export const docsPage = (_c: Ctx) => sitePage(docs);
export const agentsPage = (_c: Ctx) => sitePage(agents);
export const sharingPage = (_c: Ctx) => sitePage(sharing);
export const privacyPage = (_c: Ctx) => sitePage(privacy);
export const securityPage = (_c: Ctx) => sitePage(security);
