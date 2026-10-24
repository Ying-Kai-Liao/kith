/**
 * The blob behind a sent image's preview. Never a type the browser would render as a document: "open image in new tab"
 * on a blob: URL runs on this page's origin, so a file that is not really an image must not become a page there.
 */
export const ATTACHMENT_BLOB_TYPE = 'application/octet-stream'

export const attachmentBlob = (data: Uint8Array | ArrayBuffer): Blob => new Blob([data as BlobPart], { type: ATTACHMENT_BLOB_TYPE })
