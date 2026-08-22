<script>
  import LoginSection from '../components/LoginSection.svelte';
  import UserCard from '../components/UserCard.svelte';

  export let loggedIn = false;
  export let busy = false;
  export let userInfo = null;
  export let userInfoError = null;
  export let onLogin = null; // (username, password) => Promise
  export let loginDebug = false;
  export let onToggleLoginDebug = null; // (bool) => Promise
</script>

<UserCard {userInfo} {userInfoError} />

<LoginSection {loggedIn} {busy} {onLogin} {loginDebug} {onToggleLoginDebug} />
