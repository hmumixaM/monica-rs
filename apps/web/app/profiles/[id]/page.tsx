import { useParams } from "react-router-dom";
import { AppShell } from "../../../components/app-shell";
import { ProfileView } from "../../../components/profile/profile-page";
export default function ProfilePage() {
  const { id } = useParams<{id: string}>();
  return <AppShell active="profiles"><ProfileView profileId={id!} /></AppShell>;
}
