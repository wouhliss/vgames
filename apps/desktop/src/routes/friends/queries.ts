import { useQuery } from "@tanstack/react-query";
import { commands } from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";

export function useFriends() {
  return useQuery({
    queryKey: queryKeys.friends,
    queryFn: async () => unwrap(await commands.friendsList()),
  });
}

export function useSocialConnection() {
  return useQuery({
    queryKey: queryKeys.socialConnection,
    queryFn: () => commands.socialConnection(),
  });
}

export function useConversations() {
  return useQuery({
    queryKey: queryKeys.conversations,
    queryFn: async () => unwrap(await commands.conversationsList()),
  });
}

export function useInvites() {
  return useQuery({
    queryKey: queryKeys.invites,
    queryFn: async () => unwrap(await commands.invitesList()),
  });
}
