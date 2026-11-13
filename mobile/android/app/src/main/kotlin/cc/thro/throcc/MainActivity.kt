package cc.thro.throcc

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import cc.thro.throcc.core.Command

class MainActivity : ComponentActivity() {
    private lateinit var model: AppModel

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        model = (application as ThroccApp).open(intent)
        setContent { App(model) }
    }

    override fun onStart() {
        super.onStart()
        model.core.command(Command.Reconnect)
        (application as ThroccApp).startVoice()
    }
}
