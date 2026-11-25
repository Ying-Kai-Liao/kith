# Signing the Windows exes

`release.yml`'s `build-windows` job signs every exe in the Windows zip (the programs in `scripts/windows-exes.txt`) with
Azure Artifact Signing (Microsoft renamed it from Trusted Signing; the Azure resource provider is still
`Microsoft.CodeSigning`). It signs before the zip and its SHA-256 are made, with an RFC 3161 timestamp, then checks each
signature with `Get-AuthenticodeSignature`.

- None of the six values below set: the job prints `unsigned: Azure signing not configured` and ships an unsigned zip (green).
- All six set: every signing step must pass, or the job is red. A wrong value fails in Azure login or the signing action.
- Some set: red, "half configured". Never a half-signed zip.
- Auth is OIDC (`azure/login`), no client secret. Only the Windows job has `id-token: write`.
- Not covered: the desktop app and its NSIS installer. W16 signs those later with the same account.

## GitHub settings (names only)

Repo secrets: `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`.
Repo variables: `AZURE_SIGNING_ENDPOINT` (for example `https://eus.codesigning.azure.net`, matching the account's region),
`AZURE_SIGNING_ACCOUNT` (the signing account name), `AZURE_SIGNING_PROFILE` (the certificate profile name).

## Azure setup (done by hand, once)

1. Create an Azure subscription (pay-as-you-go) and a resource group. Register the `Microsoft.CodeSigning` resource
   provider on the subscription (Subscriptions > Resource providers).
2. Create an Artifact Signing account (portal: "Artifact Signing Accounts" > Create): pick the Basic tier and a region
   from the supported list; the region sets the endpoint URL (East US is `https://eus.codesigning.azure.net`). Name: 3 to
   24 alphanumerics, globally unique.
3. Give yourself the "Artifact Signing Identity Verifier" role on the account, then create a Public identity validation.
   Public Trust for an individual developer is only open to the US and Canada. From Australia use an Organization
   validation: a legal business entity (a company, or a sole trader with an ABN and public records that match), a
   website, and monitored emails on the business domain. Processing is 1 to 30 business days, so start it first.
4. Once validation is Completed, create a certificate profile of type Public Trust on the account (name 5 to 100 chars).
5. Create a Microsoft Entra app registration. Add a federated credential (GitHub Actions, repo `Ying-Kai-Liao/kith`).
   release.yml runs on `v*` tag pushes (subject `repo:Ying-Kai-Liao/kith:ref:refs/tags/v*`; plain federated credentials
   match the subject exactly, so use a flexible federated credential with the claims matching expression
   `claims['sub'] matches 'repo:Ying-Kai-Liao/kith:ref:refs/tags/*'`) and on manual dispatch (subject
   `repo:Ying-Kai-Liao/kith:ref:refs/heads/main` for dispatches from main; add it as a second credential).
6. On the signing account (or the certificate profile), assign the app registration the "Artifact Signing Certificate
   Profile Signer" role (shown as "Trusted Signing Certificate Profile Signer" in older portal builds).
7. Add the three secrets and three variables above in GitHub (Settings > Secrets and variables > Actions). The client,
   tenant and subscription IDs are not secret in Azure's eyes but are kept as secrets here.

## Cost

Basic tier: 5,000 signatures a month included. Microsoft's pricing page
(https://azure.microsoft.com/en-us/pricing/details/artifact-signing/) rendered no price when this was written; Basic was
announced at USD 9.99 a month, so check the page before subscribing. A release signs about four files.

Docs: https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart and
https://github.com/Azure/artifact-signing-action
