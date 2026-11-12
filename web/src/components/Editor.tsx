// Rich text for descriptions and comments (Tiptap): formatting, task lists, links, @people and
// #issue references. Stored as ProseMirror JSON.
import { t } from '../i18n'
import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from 'react'
import { useEditor, EditorContent, ReactRenderer, type Editor } from '@tiptap/react'
import StarterKit from '@tiptap/starter-kit'
import { TaskList, TaskItem } from '@tiptap/extension-list'
import { Placeholder } from '@tiptap/extensions'
import Mention from '@tiptap/extension-mention'
import type { SuggestionKeyDownProps, SuggestionOptions, SuggestionProps } from '@tiptap/suggestion'
import { Bold, Italic, Strikethrough, Code, List, ListOrdered, ListTodo, Quote, SquareCode, Link as LinkIcon } from 'lucide-react'
import { api, type Issue, type Person } from '../api'
import { Avatar } from '../ui'
import { TypeIcon } from '../icons'
import { queryClient } from '../queryClient'

interface Item {
  id: string
  label: string
  sub?: string
  issue?: Issue
}

interface ListHandle {
  onKeyDown: (p: SuggestionKeyDownProps) => boolean
}

const SuggestList = forwardRef<ListHandle, { items: Item[]; command: (i: { id: string; label: string }) => void; char: string }>(function SuggestList({ items, command, char }, ref) {
  const [sel, setSel] = useState(0)
  useEffect(() => setSel(0), [items])
  useImperativeHandle(ref, () => ({
    onKeyDown: ({ event }) => {
      if (event.key === 'ArrowDown') {
        setSel((s) => (s + 1) % Math.max(items.length, 1))
        return true
      }
      if (event.key === 'ArrowUp') {
        setSel((s) => (s - 1 + items.length) % Math.max(items.length, 1))
        return true
      }
      if (event.key === 'Enter' || event.key === 'Tab') {
        const it = items[sel]
        if (it) command({ id: it.id, label: it.label })
        return true
      }
      return false
    },
  }))
  if (!items.length) return <div className="suggest-pop"><div className="menu-empty">{char === '@' ? t('No one found') : t('No issues found')}</div></div>
  return (
    <div className="suggest-pop" role="listbox">
      {items.map((it, i) => (
        <button
          type="button"
          key={it.id}
          className={`menu-item ${i === sel ? 'hover' : ''}`}
          onMouseEnter={() => setSel(i)}
          onMouseDown={(e) => {
            e.preventDefault()
            command({ id: it.id, label: it.label })
          }}
        >
          {it.issue ? <TypeIcon icon={it.issue.type.icon} color={it.issue.type.color} /> : <Avatar name={it.label} id={it.id} size={20} />}
          <span className={it.issue ? 'issue-key' : ''}>{it.issue ? it.issue.key : it.label}</span>
          <span className="mi-desc" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{it.sub}</span>
        </button>
      ))}
    </div>
  )
})

function renderer(char: string): SuggestionOptions<Item>['render'] {
  return () => {
    let component: ReactRenderer<ListHandle> | null = null
    let host: HTMLDivElement | null = null
    const place = (props: SuggestionProps<Item>) => {
      const rect = props.clientRect?.()
      if (!rect || !host) return
      const h = Math.min(260, host.offsetHeight || 200)
      host.style.position = 'fixed'
      host.style.zIndex = '1200'
      host.style.left = `${Math.min(rect.left, window.innerWidth - 370)}px`
      host.style.top = `${rect.bottom + 6 + h > window.innerHeight ? Math.max(8, rect.top - h - 6) : rect.bottom + 6}px`
    }
    return {
      onStart: (props) => {
        component = new ReactRenderer(SuggestList, { props: { ...props, char }, editor: props.editor })
        host = document.createElement('div')
        host.appendChild(component.element)
        document.body.appendChild(host)
        place(props)
      },
      onUpdate: (props) => {
        component?.updateProps({ ...props, char })
        place(props)
      },
      onKeyDown: (props) => {
        if (props.event.key === 'Escape') {
          host?.remove()
          host = null
          return true
        }
        return component?.ref?.onKeyDown(props) ?? false
      },
      onExit: () => {
        host?.remove()
        component?.destroy()
        host = null
        component = null
      },
    }
  }
}

async function people(query: string): Promise<Item[]> {
  const list = await queryClient.fetchQuery({ queryKey: ['people'], queryFn: () => api.get<Person[]>('/people'), staleTime: 120_000 })
  const q = query.toLowerCase()
  return list
    .filter((p) => p.name.toLowerCase().includes(q) || (p.email ?? '').toLowerCase().includes(q))
    .slice(0, 8)
    .map((p) => ({ id: p.id, label: p.name, sub: p.title ?? p.email ?? '' }))
}

async function issues(query: string): Promise<Item[]> {
  const list = await api.get<Issue[]>(`/quick?q=${encodeURIComponent(query)}`).catch(() => [] as Issue[])
  return list.slice(0, 8).map((i) => ({ id: i.key, label: i.key, sub: i.title, issue: i }))
}

export function extensions(placeholder?: string) {
  return [
    StarterKit.configure({ link: { openOnClick: false, autolink: true, HTMLAttributes: { rel: 'noopener noreferrer nofollow', target: '_blank' } }, heading: { levels: [1, 2, 3] } }),
    TaskList,
    TaskItem.configure({ nested: true }),
    Placeholder.configure({ placeholder: placeholder ?? '' }),
    Mention.configure({
      HTMLAttributes: { class: 'mention' },
      renderText: ({ node }) => (node.attrs.mentionSuggestionChar === '#' ? node.attrs.id : `@${node.attrs.label ?? ''}`),
      renderHTML: ({ node, options }) => [
        'span',
        { ...options.HTMLAttributes, 'data-type': 'mention', 'data-id': node.attrs.id, 'data-mention-suggestion-char': node.attrs.mentionSuggestionChar },
        node.attrs.mentionSuggestionChar === '#' ? node.attrs.id : `@${node.attrs.label ?? node.attrs.id}`,
      ],
      suggestions: [
        { char: '@', items: ({ query }) => people(query), render: renderer('@') },
        { char: '#', items: ({ query }) => issues(query), render: renderer('#') },
      ],
    }),
  ]
}

export const EMPTY_DOC = { type: 'doc', content: [{ type: 'paragraph' }] }

export function isEmptyDoc(doc: any): boolean {
  if (!doc?.content?.length) return true
  return doc.content.every((n: any) => n.type === 'paragraph' && !n.content?.length)
}

function Toolbar({ editor }: { editor: Editor }) {
  const b = (label: string, icon: React.ReactNode, run: () => void, active: boolean) => (
    <button type="button" className={`icon-btn ${active ? 'active' : ''}`} title={label} aria-label={label} aria-pressed={active} onMouseDown={(e) => e.preventDefault()} onClick={run}>
      {icon}
    </button>
  )
  return (
    <div className="rte-toolbar">
      {b(t('Bold'), <Bold size={15} />, () => editor.chain().focus().toggleBold().run(), editor.isActive('bold'))}
      {b(t('Italic'), <Italic size={15} />, () => editor.chain().focus().toggleItalic().run(), editor.isActive('italic'))}
      {b(t('Strikethrough'), <Strikethrough size={15} />, () => editor.chain().focus().toggleStrike().run(), editor.isActive('strike'))}
      {b(t('Code'), <Code size={15} />, () => editor.chain().focus().toggleCode().run(), editor.isActive('code'))}
      {b(t('Bullet list'), <List size={15} />, () => editor.chain().focus().toggleBulletList().run(), editor.isActive('bulletList'))}
      {b(t('Numbered list'), <ListOrdered size={15} />, () => editor.chain().focus().toggleOrderedList().run(), editor.isActive('orderedList'))}
      {b(t('Checklist'), <ListTodo size={15} />, () => editor.chain().focus().toggleTaskList().run(), editor.isActive('taskList'))}
      {b(t('Quote'), <Quote size={15} />, () => editor.chain().focus().toggleBlockquote().run(), editor.isActive('blockquote'))}
      {b(t('Code block'), <SquareCode size={15} />, () => editor.chain().focus().toggleCodeBlock().run(), editor.isActive('codeBlock'))}
      {b(
        t('Link'),
        <LinkIcon size={15} />,
        () => {
          const prev = editor.getAttributes('link').href as string | undefined
          const url = window.prompt(t('Link address'), prev ?? 'https://')
          if (url === null) return
          if (!url || url === 'https://') editor.chain().focus().unsetLink().run()
          else if (/^(https?:|mailto:|\/)/i.test(url)) editor.chain().focus().extendMarkRange('link').setLink({ href: url }).run()
        },
        editor.isActive('link'),
      )}
    </div>
  )
}

/** Opens `#KEY` references as issues. */
function onMentionClick(e: React.MouseEvent) {
  const el = (e.target as HTMLElement).closest('.mention[data-mention-suggestion-char="#"]') as HTMLElement | null
  if (!el?.dataset.id) return
  e.preventDefault()
  const url = new URL(window.location.href)
  url.searchParams.set('issue', el.dataset.id)
  window.history.pushState({}, '', url)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

/** Read-only rich text. */
export function RichView({ doc, label }: { doc: any; label?: string }) {
  const editor = useEditor(
    { extensions: extensions(), content: doc ?? EMPTY_DOC, editable: false, immediatelyRender: true, editorProps: { attributes: { 'aria-label': label ?? t('Text'), 'aria-readonly': 'true' } } },
    [JSON.stringify(doc)],
  )
  return (
    <div onClick={onMentionClick}>
      <EditorContent editor={editor} />
    </div>
  )
}

/**
 * Click-to-edit rich text with Save / Cancel (descriptions), or an always-open box when `boxed`
 * (new comments, the create dialog).
 */
export function RichText({
  value,
  onSave,
  onChange,
  editable = true,
  placeholder,
  boxed,
  autoFocus,
  saveLabel,
  emptyText,
  label,
}: {
  value: any
  onSave?: (doc: any) => Promise<void> | void
  onChange?: (doc: any) => void
  editable?: boolean
  placeholder?: string
  boxed?: boolean
  autoFocus?: boolean
  saveLabel?: string
  emptyText?: string
  /** what the text is, for screen readers (defaults to the placeholder) */
  label?: string
}) {
  const [editing, setEditing] = useState(!!boxed)
  const [busy, setBusy] = useState(false)
  const initial = useRef(value)
  const editor = useEditor(
    {
      extensions: extensions(placeholder),
      content: value ?? EMPTY_DOC,
      editable: editing,
      immediatelyRender: true,
      autofocus: autoFocus ? 'end' : false,
      onUpdate: ({ editor }) => onChange?.(editor.getJSON()),
      editorProps: { attributes: { 'aria-label': label ?? placeholder ?? t('Text'), 'aria-multiline': 'true' } },
    },
    [],
  )
  useEffect(() => {
    editor?.setEditable(editing)
  }, [editing, editor])
  // a new value from outside (another person saved) while not editing
  useEffect(() => {
    if (!editor || editing) return
    if (JSON.stringify(value) !== JSON.stringify(editor.getJSON())) editor.commands.setContent(value ?? EMPTY_DOC)
    initial.current = value
  }, [value, editor, editing])
  if (!editor) return null
  const save = async () => {
    if (!onSave) return
    setBusy(true)
    try {
      await onSave(editor.getJSON())
      if (!boxed) setEditing(false)
      else editor.commands.setContent(EMPTY_DOC)
    } finally {
      setBusy(false)
    }
  }
  const empty = !editing && isEmptyDoc(value)
  return (
    <div
      className={`rte ${editable ? 'editable' : ''} ${editing ? 'editing' : ''} ${boxed ? 'boxed' : ''}`}
      onClick={(e) => {
        onMentionClick(e)
        if (editable && !editing && !(e.target as HTMLElement).closest('a, .mention')) {
          setEditing(true)
          setTimeout(() => editor.commands.focus('end'), 0)
        }
      }}
      onKeyDown={(e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === 'Enter' && onSave) {
          e.preventDefault()
          void save()
        }
        if (e.key === 'Escape' && !boxed && editing) {
          e.stopPropagation()
          editor.commands.setContent(initial.current ?? EMPTY_DOC)
          setEditing(false)
        }
      }}
    >
      {editing && <Toolbar editor={editor} />}
      {empty ? <div className="faint" style={{ padding: '2px 0' }}>{emptyText ?? (editable ? t('Add a description…') : t('No description'))}</div> : <EditorContent editor={editor} />}
      {editing && onSave && (
        <div className="row" style={{ marginTop: 8 }}>
          <button className="btn btn-primary btn-sm" disabled={busy} onClick={(e) => { e.stopPropagation(); void save() }}>{saveLabel ?? t('Save')}</button>
          {!boxed && (
            <button
              className="btn btn-ghost btn-sm"
              onClick={(e) => {
                e.stopPropagation()
                editor.commands.setContent(initial.current ?? EMPTY_DOC)
                setEditing(false)
              }}
            >{t('Cancel')}</button>
          )}
          <span className="faint small">{t('Ctrl+Enter to save · @ mentions · # issues')}</span>
        </div>
      )}
    </div>
  )
}
