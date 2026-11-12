package cc.thro.throcc

import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import cc.thro.throcc.core.BlockedPerson
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.Connection
import cc.thro.throcc.core.Core
import cc.thro.throcc.core.CoreException
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.Event
import cc.thro.throcc.core.KnownServer
import cc.thro.throcc.core.Listener
import cc.thro.throcc.core.Palette
import cc.thro.throcc.core.Profile
import cc.thro.throcc.core.User
import cc.thro.throcc.core.VerifiedPerson
import cc.thro.throcc.core.customThemeName
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write
import kotlinx.coroutines.CoroutineScope
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

/** How saving the server's name and picture went. */
sealed interface ServerSave {
    data object Saving : ServerSave
    data object Saved : ServerSave
    data class Refused(val reason: String) : ServerSave
}

/** The name of the theme made of the user's own colours. */
val CUSTOM = customThemeName()

fun CoreState.connectedTo(address: String) = connection == Connection.Connected && server?.address == address

class AppModel(val core: Core, sample: String?) : ViewModel() {
    private val shown = MutableStateFlow(core.state())
    val state = shown.asStateFlow()

    /** The pages opened on top of the server list, the last one in front. */
    val stack = mutableStateListOf(Screen.Servers)

    /** Whether a narrow window shows our room in front of the pages. */
    var roomInFront by mutableStateOf(false)

    var sheetPerson by mutableStateOf<ULong?>(null)

    var profile by mutableStateOf(core.profile())
        private set
    var theme by mutableStateOf(core.theme())
        private set
    var servers by mutableStateOf(emptyList<KnownServer>())
        private set

    /** Everyone verified on this install, once read. */
    var verified by mutableStateOf<List<VerifiedPerson>?>(null)
        private set

    /** Everyone blocked on this install, once read. */
    var blocked by mutableStateOf<List<BlockedPerson>?>(null)
        private set

    var connectingTo by mutableStateOf<String?>(null)
        private set
    var users by mutableStateOf(emptyList<User>())
        private set
    var invite by mutableStateOf<String?>(null)
        private set
    var passwordRefused by mutableStateOf<String?>(null)

    var serverSaved by mutableStateOf<ServerSave?>(null)

    val notices = Channel<String>(Channel.BUFFERED)
    val snackbars = SnackbarHostState()

    private val pictures = mutableStateMapOf<String, Picture>()
    private var serverPictures by mutableStateOf(emptyMap<String, Picture>())
    private var blockedPictures by mutableStateOf(emptyMap<String, Picture>())

    /** The connection a sample shows in place of the mock's. */
    private var sampleConnection: Connection? = null

    private val coreHandle = ReentrantReadWriteLock()
    private var closed = false

    private val listener = object : Listener {
        override fun state(state: CoreState) {
            unlessClosed { loadPictures(state) }
            viewModelScope.launch { show(state) }
        }

        override fun event(event: Event) {
            if (event is Event.AvatarReady) {
                unlessClosed { loadPicture(event.hash) }
            } else {
                viewModelScope.launch { handle(event) }
            }
        }
    }

    init {
        core.setListener(listener)
        viewModelScope.launch { loadServers() }
        sample?.let(::showSample)
    }

    override fun onCleared() {
        core.setListener(null)
        CoroutineScope(Dispatchers.IO).launch {
            coreHandle.write {
                closed = true
                core.close()
            }
        }
    }

    fun open(screen: Screen) {
        roomInFront = false
        if (screen == Screen.Server) serverSaved = null
        if (screen !in stack) stack += screen
    }

    fun back() {
        if (stack.size > 1) stack.removeAt(stack.lastIndex)
    }

    /** Opens the rooms of the server at `address`, signing in unless we are connected there. */
    fun openServer(address: String) {
        if (state.value.connectedTo(address)) open(Screen.Rooms) else connect(address)
    }

    /** Signs in at `address` and opens its rooms, or says why it couldn't. */
    fun connect(address: String, invite: String? = null) {
        if (connectingTo != null) return
        viewModelScope.launch {
            connectingTo = address
            val signedIn = signIn(address, invite)
            connectingTo = null
            when {
                signedIn -> showPages(Screen.Rooms)
                Screen.Rooms in stack -> showPages()
            }
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
            ask { core.disconnect() }
            loadServers()
        }
    }

    fun forgetServer(address: String) {
        viewModelScope.launch {
            ask { core.forgetServer(address) }
            loadServers()
        }
    }

    /** Our name and picture are kept and the profile page closes, or a notice says why not. */
    fun saveProfile(name: String, picture: ByteArray?) {
        viewModelScope.launch {
            val saved = ask { core.setProfile(Profile(name, picture)) } != null
            profile = ask { core.profile() }
            if (saved) stack.remove(Screen.Profile)
        }
    }

    /** Picks the preset called `name`, or with `CUSTOM` the user's own colours. */
    fun pickTheme(name: String) {
        changeTheme { if (name == CUSTOM) core.setCustomTheme(core.customPalette()) else core.setTheme(name) }
    }

    fun pickCustomTheme(palette: Palette) {
        changeTheme { core.setCustomTheme(palette) }
    }

    fun loadVerified() {
        viewModelScope.launch { ask { core.verified() }?.let { verified = it } }
    }

    fun unverify(key: ByteArray) {
        viewModelScope.launch {
            ask { core.unverify(key) }
            loadVerified()
        }
    }

    fun loadBlocked() {
        viewModelScope.launch {
            val kept = blockedPictures
            val (people, peoplePictures) = ask {
                val people = core.blocked()
                people to decoded(people.map { it.key to it.picture }, kept)
            } ?: return@launch
            blocked = people
            blockedPictures = peoplePictures
        }
    }

    fun unblock(key: ByteArray) {
        viewModelScope.launch {
            ask { core.unblock(key) }
            loadBlocked()
        }
    }

    /** The decoded picture behind `hash`, once it has arrived. */
    fun picture(hash: ByteArray?): ImageBitmap? = hash?.let { pictures[it.toHexString()]?.image }

    /** The picture file behind `hash`, once it has arrived. */
    fun pictureFile(hash: ByteArray?): ByteArray? = hash?.let { pictures[it.toHexString()]?.file }

    fun picture(server: KnownServer): ImageBitmap? = serverPictures[server.fingerprint.toHexString()]?.image

    fun picture(person: BlockedPerson): ImageBitmap? = blockedPictures[person.key.toHexString()]?.image

    /** What `action` gave off the main thread, or null where it failed, which shows as a notice. */
    private suspend fun <T> ask(action: () -> T): T? = withContext(Dispatchers.IO) {
        unlessClosed {
            try {
                action()
            } catch (failure: CoreException) {
                notices.trySend(failure.message.orEmpty())
                null
            }
        }
    }

    /** The result of `action`, run while the core's handle is held open, or null once the handle is closed. */
    private fun <T> unlessClosed(action: () -> T): T? = coreHandle.read { if (closed) null else action() }

    private fun changeTheme(change: () -> Unit) {
        viewModelScope.launch {
            ask(change)
            ask { core.theme() }?.let { theme = it }
        }
    }

    private fun show(state: CoreState) {
        if (state.current?.room != shown.value.current?.room) roomInFront = state.current != null
        if (state.people.none { it.user == sheetPerson }) sheetPerson = null
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
            Event.ServerProfileSaved -> if (serverSaved == ServerSave.Saving) serverSaved = ServerSave.Saved
            is Event.ServerProfileRefused -> serverSaved = ServerSave.Refused(event.reason)
            is Event.ProfileRefused -> notices.trySend("This server refused your profile: ${event.reason}")
            Event.ProfileSaved, is Event.AvatarReady -> {}
        }
    }

    /** Leaves only `pages` open on top of the server list. */
    private fun showPages(vararg pages: Screen) {
        stack.removeRange(1, stack.size)
        stack += pages
    }

    /** Whether signing in at `address` worked. A failure shows as a notice. */
    private suspend fun signIn(address: String, invite: String?): Boolean {
        sampleConnection = null
        users = emptyList()
        this.invite = null
        val signedIn = ask { core.connect(address, invite) } != null
        show(core.state())
        loadServers()
        return signedIn
    }

    private suspend fun loadServers() {
        val kept = serverPictures
        val (listed, listedPictures) = ask {
            val listed = core.servers()
            listed to decoded(listed.map { it.fingerprint to it.picture }, kept)
        } ?: return
        servers = listed
        serverPictures = listedPictures
    }

    private fun loadPictures(state: CoreState) {
        val named = (state.people.map { it.avatar } + state.server?.avatar).filterNotNull()
        pictures.keys.retainAll(named.map { it.toHexString() }.toSet())
        named.forEach(::loadPicture)
    }

    private fun loadPicture(hash: ByteArray) {
        val key = hash.toHexString()
        if (key in pictures) return
        core.avatar(hash)?.let { pictures[key] = Picture(it) }
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
                "person" -> sheetPerson = 2u
                "personblocked" -> sheetPerson = 3u
                "invites" -> core.command(Command.CreateInvite)
                "reconnect" -> sampleConnection = Connection.AwaitingReconnect
                "elsewhere" -> sampleConnection = Connection.SignedInElsewhere
            }
            show(core.state())
        }
    }
}
