// Single sign-on on the login page (a button per provider) and how people sign in (Admin → Users,
// Profile & settings → Password).
import { t } from '../i18n'
import { useQuery } from '@tanstack/react-query'
import { Building, KeyRound, Lock } from 'lucide-react'
import { api } from '../api'
import './Sso.css'

export interface SignInProvider {
  id: string
  name: string
  type: 'oidc' | 'github' | 'ldap' | 'saml' | null
  preset?: string | null
  /** redirect-based providers; LDAP signs in through the password form */
  loginUrl?: string | null
}

export interface SignInOptions {
  /** local passwords sign in (off with TASKCRAFT_DISABLE_PASSWORD_LOGIN) */
  passwordLogin: boolean
  providers: SignInProvider[]
}

/** How one person signs in. */
export interface SignInMethods {
  password: boolean
  providers: SignInProvider[]
}

export function useSignInOptions() {
  return useQuery({ queryKey: ['auth-providers'], queryFn: () => api.get<SignInOptions>('/auth/providers'), staleTime: 60_000 })
}

/** The provider's label; the names the server makes up when none is configured are translated. */
export function providerName(p: Pick<SignInProvider, 'name'>): string {
  switch (p.name) {
    case 'Single sign-on':
      return t('Single sign-on')
    case 'Directory (LDAP)':
      return t('Directory (LDAP)')
    case 'SAML single sign-on':
      return t('SAML single sign-on')
    default:
      return p.name
  }
}

function ProviderIcon({ p }: { p: SignInProvider }) {
  if (p.preset === 'microsoft')
    return (
      <svg width="15" height="15" viewBox="0 0 21 21" aria-hidden="true">
        <rect x="1" y="1" width="9" height="9" fill="#f25022" />
        <rect x="11" y="1" width="9" height="9" fill="#7fba00" />
        <rect x="1" y="11" width="9" height="9" fill="#00a4ef" />
        <rect x="11" y="11" width="9" height="9" fill="#ffb900" />
      </svg>
    )
  if (p.type === 'ldap') return <Building size={15} aria-hidden />
  return <KeyRound size={15} aria-hidden />
}

/** "Continue with …" buttons and the divider to the password form below them. */
export function SsoButtons({ options, next, passwordForm }: { options: SignInOptions; next: string | null; passwordForm: boolean }) {
  const redirects = options.providers.filter((p) => p.loginUrl)
  const directory = options.providers.filter((p) => p.type === 'ldap')
  if (redirects.length === 0) return null
  const suffix = next ? `?next=${encodeURIComponent(next)}` : ''
  return (
    <>
      <div className="sso-buttons">
        {redirects.map((p) => (
          <a key={p.id} className="btn sso-btn" href={`${p.loginUrl}${suffix}`}>
            <ProviderIcon p={p} /> {t('Continue with {name}', { name: providerName(p) })}
          </a>
        ))}
      </div>
      {passwordForm && (
        <div className="sso-divider faint small">
          {directory.length === 1
            ? t('or with your {name} account', { name: providerName(directory[0]) })
            : directory.length > 1
              ? t('or with your directory account')
              : t('or with your password')}
        </div>
      )}
    </>
  )
}

/** Badges: password and / or the providers someone signs in with. */
export function SignInBadges({ methods }: { methods: SignInMethods | null | undefined }) {
  if (!methods) return null
  if (!methods.password && methods.providers.length === 0) return <span className="faint">{t('Not yet')}</span>
  return (
    <span className="sso-badges">
      {methods.password && (
        <span className="badge" title={t('Signs in with a password')}>
          <Lock size={11} aria-hidden /> {t('Password')}
        </span>
      )}
      {methods.providers.map((p) => (
        <span key={p.id} className="badge badge-accent" title={t('Signs in with {name}', { name: providerName(p) })}>
          {p.type === 'ldap' ? <Building size={11} aria-hidden /> : <KeyRound size={11} aria-hidden />} {providerName(p)}
        </span>
      ))}
    </span>
  )
}
