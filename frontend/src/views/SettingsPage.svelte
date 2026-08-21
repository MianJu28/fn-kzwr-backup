<script>
  import LoginSection from '../components/LoginSection.svelte';

  export let loggedIn = false;
  export let busy = false;
  export let onLogin = null; // (username, password) => Promise
</script>

<LoginSection {loggedIn} {busy} {onLogin} />
