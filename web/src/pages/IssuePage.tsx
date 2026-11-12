import { useParams } from 'react-router-dom'
import IssueView from '../components/IssueView'

export default function IssuePage() {
  const { key } = useParams()
  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      <IssueView key={key} issueKey={key ?? ''} />
    </div>
  )
}
