package cc.thro.throcc

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.Connection
import cc.thro.throcc.core.Core
import cc.thro.throcc.core.CoreException
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.Event
import cc.thro.throcc.core.KnownServer
import cc.thro.throcc.core.Listener
import cc.thro.throcc.core.Profile
import cc.thro.throcc.core.User
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class Screen {
    Servers, AddServer, Rooms, Settings, Profile, Appearance, Verified, Blocked, About, Server, Members, Invites, Keys,

    /** Our room, which a narrow window shows in front of the pages. It is never on the model's stack. */
    Room,
}

const val SAVING = "Saving…"

class AppModel(val core: Core, sample: String?) : ViewModel(), Listener {
    private val shown = MutableStateFlow(core.state())
    val state = shown.asStateFlow()

    /** The pages opened on top of the server list, the last one in front. */
    val stack = mutableStateListOf(Screen.Servers)

    /** Whether a narrow window shows our room in front of the pages. */
    var roomInFront by mutableStateOf(false)

    /** Whose sheet is open. */
    var sheet by mutableStateOf<ULong?>(null)

    var profile by mutableStateOf(core.profile())
        private set
    var accent by mutableStateOf(core.accent())
        private set
    var servers by mutableStateOf(emptyList<KnownServer>())
        private set

    /** The address being signed in to. */
    var connecting by mutableStateOf<String?>(null)
        private set
    var users by mutableStateOf(emptyList<User>())
        private set
    var invite by mutableStateOf<String?>(null)
        private set
    var passwordRefused by mutableStateOf<String?>(null)

    /** How saving the server's name and picture went: `SAVING`, "Saved" or why it was refused. */
    var serverSaved by mutableStateOf<String?>(null)

    val notices = Channel<String>(Channel.BUFFERED)

    private val pictures = mutableStateMapOf<String, ImageBitmap>()

    /** The connection a sample shows in place of the mock's. */
    private var sampleConnection: Connection? = null

    init {
        core.setListener(this)
        viewModelScope.launch { loadServers() }
        sample?.let(::showSample)
    }

    override fun onCleared() {
        core.setListener(null)
        core.close()
    }

    override fun state(state: CoreState) {
        loadPictures(state)
        viewModelScope.launch { show(state) }
    }

    override fun event(event: Event) {
        if (event is Event.AvatarReady) loadPicture(event.hash) else viewModelScope.launch { handle(event) }
    }

    fun open(screen: Screen) {
        roomInFront = false
        if (screen !in stack) stack += screen
    }

    fun back() {
        if (stack.size > 1) stack.removeAt(stack.lastIndex)
    }

    /** Opens the rooms of the server at `address`, signing in unless we are connected there. */
    fun openServer(address: String) {
        if (state.value.server?.address == address) open(Screen.Rooms) else connect(address)
    }

    /** Signs in at `address` and opens its rooms, or says why it couldn't. */
    fun connect(address: String, invite: String? = null) {
        if (connecting != null) return
        viewModelScope.launch {
            connecting = address
            val failure = signIn(address, invite)
            connecting = null
            when {
                failure == null -> showPages(Screen.Rooms)
                Screen.Rooms in stack -> showPages()
            }
            failure?.let(notices::trySend)
        }
    }

    fun reconnect() {
        sampleConnection = null
        core.command(Command.Reconnect)
        show(core.state())
    }

    /** Ends the connection and returns to the server list. */
    fun disconnect() {
        showPages()
        viewModelScope.launch {
            withContext(Dispatchers.IO) { core.disconnect() }
            loadServers()
        }
    }

    fun forgetServer(address: String) {
        tell { core.forgetServer(address) }
        viewModelScope.launch { loadServers() }
    }

    fun saveProfile(name: String, picture: ByteArray?): Boolean {
        val saved = tell { core.setProfile(Profile(name, picture)) }
        profile = core.profile()
        return saved
    }

    fun pickAccent(name: String) {
        tell { core.setAccent(name) }
        accent = core.accent()
    }

    fun pickCustomAccent(rgb: UInt) {
        tell { core.setCustomAccent(rgb) }
        accent = core.accent()
    }

    /** Whether `action` on the core worked. A failure shows as a notice. */
    fun tell(action: () -> Unit): Boolean = try {
        action()
        true
    } catch (failure: CoreException) {
        notices.trySend(failure.message.orEmpty())
        false
    }

    /** The decoded picture behind `hash`, once it has arrived. */
    fun picture(hash: ByteArray?): ImageBitmap? = hash?.let { pictures[it.toHexString()] }

    private fun show(state: CoreState) {
        if (state.current?.room != shown.value.current?.room) roomInFront = state.current != null
        if (state.people.none { it.user == sheet }) sheet = null
        shown.value = sampleConnection?.let { state.copy(connection = it) } ?: state
    }

    private fun handle(event: Event) {
        when (event) {
            is Event.Invite -> invite = event.code
            is Event.Users -> users = event.users
            is Event.Notice -> notices.trySend(event.text)
            is Event.RemovedFromServer -> {
                notices.trySend(event.reason)
                disconnect()
            }
            Event.LeftServer -> disconnect()
            is Event.PasswordRefused -> passwordRefused = event.reason
            Event.ServerProfileSaved -> if (serverSaved == SAVING) serverSaved = "Saved"
            is Event.ServerProfileRefused -> serverSaved = event.reason
            is Event.ProfileRefused -> notices.trySend("This server refused your profile: ${event.reason}")
            Event.ProfileSaved, is Event.AvatarReady -> {}
        }
    }

    /** Leaves only `pages` open on top of the server list. */
    private fun showPages(vararg pages: Screen) {
        stack.removeRange(1, stack.size)
        stack += pages
    }

    private suspend fun signIn(address: String, invite: String?): String? {
        sampleConnection = null
        users = emptyList()
        this.invite = null
        val failure = withContext(Dispatchers.IO) {
            try {
                core.connect(address, invite)
                // The core's first state arrives before it hands out pictures.
                loadPictures(core.state())
                null
            } catch (failure: CoreException) {
                failure.message.orEmpty()
            }
        }
        show(core.state())
        loadServers()
        return failure
    }

    private suspend fun loadServers() {
        withContext(Dispatchers.IO) { tell { servers = core.servers() } }
    }

    private fun loadPictures(state: CoreState) {
        (state.people.map { it.avatar } + state.server?.avatar).filterNotNull().forEach(::loadPicture)
    }

    private fun loadPicture(hash: ByteArray) {
        val key = hash.toHexString()
        if (key in pictures) return
        core.avatar(hash)?.toImage()?.let { pictures[key] = it }
    }

    /** Opens the sample `name` over the mock's server, as the `screen` extra asks. */
    private fun showSample(name: String) {
        stack += when (name) {
            "addserver" -> listOf(Screen.AddServer)
            "rooms", "room", "blocked", "person", "personblocked", "reconnect", "elsewhere" -> listOf(Screen.Rooms)
            "settings" -> listOf(Screen.Settings)
            "profile" -> listOf(Screen.Settings, Screen.Profile)
            "appearance" -> listOf(Screen.Settings, Screen.Appearance)
            "verified" -> listOf(Screen.Settings, Screen.Verified)
            "blockedpeople" -> listOf(Screen.Settings, Screen.Blocked)
            "server" -> listOf(Screen.Rooms, Screen.Server)
            "members" -> listOf(Screen.Rooms, Screen.Members)
            "invites" -> listOf(Screen.Rooms, Screen.Invites)
            "keys" -> listOf(Screen.Rooms, Screen.Keys)
            else -> emptyList()
        }
        if (name in listOf("onboarding", "servers", "addserver")) return
        viewModelScope.launch {
            signIn("owls.thro.cc", null)
            if (name in listOf("room", "blocked", "person", "personblocked")) core.command(Command.Join(1u))
            if (name in listOf("blocked", "personblocked", "blockedpeople")) core.command(Command.Block(3u))
            when (name) {
                "person" -> sheet = 2u
                "personblocked" -> sheet = 3u
                "invites" -> core.command(Command.CreateInvite)
                "reconnect" -> sampleConnection = Connection.AwaitingReconnect
                "elsewhere" -> sampleConnection = Connection.SignedInElsewhere
            }
            show(core.state())
        }
    }
}
