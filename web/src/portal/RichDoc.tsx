// Rich text in the portal: a ProseMirror document (descriptions, comments) as plain elements.
// The server already reduced it to what requesters may see (no mentions of people's accounts,
// no images, links only to the web and e-mail); React escapes the text.
import { t } from '../i18n'
import { Fragment, type ReactNode } from 'react'

interface Mark {
  type: string
  attrs?: { href?: string }
}
export interface DocNode {
  type: string
  text?: string
  marks?: Mark[]
  attrs?: { level?: number; checked?: boolean; start?: number }
  content?: DocNode[]
}

const safeHref = (h: string | undefined) => !!h && /^(https?:|mailto:)/i.test(h)

/** The text of a document (to tell an empty one). */
export function docText(n: DocNode | null | undefined): string {
  if (!n) return ''
  return (n.text ?? '') + (n.content ?? []).map(docText).join('')
}

function text(n: DocNode, key: number): ReactNode {
  let el: ReactNode = n.text ?? ''
  for (const m of n.marks ?? []) {
    if (m.type === 'bold') el = <strong>{el}</strong>
    else if (m.type === 'italic') el = <em>{el}</em>
    else if (m.type === 'strike') el = <s>{el}</s>
    else if (m.type === 'underline') el = <u>{el}</u>
    else if (m.type === 'code') el = <code>{el}</code>
    else if (m.type === 'link' && safeHref(m.attrs?.href)) el = <a href={m.attrs!.href} rel="noopener noreferrer nofollow">{el}</a>
  }
  return <Fragment key={key}>{el}</Fragment>
}

function node(n: DocNode, key: number): ReactNode {
  const kids = () => (n.content ?? []).map(node)
  switch (n.type) {
    case 'text':
      return text(n, key)
    case 'hardBreak':
      return <br key={key} />
    case 'paragraph':
      return <p key={key}>{kids()}</p>
    // headings inside a message don't take part in the page's outline
    case 'heading':
      return <p key={key} className="portal-doc-heading">{kids()}</p>
    case 'bulletList':
      return <ul key={key}>{kids()}</ul>
    case 'orderedList':
      return <ol key={key} start={n.attrs?.start}>{kids()}</ol>
    case 'listItem':
      return <li key={key}>{kids()}</li>
    case 'taskList':
      return <ul key={key} className="portal-doc-tasks">{kids()}</ul>
    case 'taskItem':
      return (
        <li key={key}>
          <span className="portal-doc-check" role="img" aria-label={n.attrs?.checked ? t('Done [[checklist item]]') : t('Open [[checklist item]]')}>
            {n.attrs?.checked ? '☑' : '☐'}
          </span>
          <div>{kids()}</div>
        </li>
      )
    case 'blockquote':
      return <blockquote key={key}>{kids()}</blockquote>
    case 'codeBlock':
      return <pre key={key}><code>{docText(n)}</code></pre>
    case 'horizontalRule':
      return <hr key={key} />
    default:
      return <Fragment key={key}>{kids()}</Fragment>
  }
}

export default function RichDoc({ doc }: { doc: DocNode | null | undefined }) {
  if (!doc) return null
  return <>{node(doc, 0)}</>
}
