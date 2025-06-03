#2000
Child Kobold Speech~
0 b 5
~
   wait 5s
   say My daddy doesn't like those lizards in the eastern cave.  They're big and scarey.
   wait 5s
   say I want my mommy.
   cry
   wait 10s
   say Who are you?  You aren't here to hurt us, are you?
~
#2001
Kobold Guard speech~
0 g 25
~
if %actor.vnum% > 0
   halt
   %random.10% == %pos%
elseif %actor.align% > 0 & %actor.cha% > 13 & %pos% > 5
   wait 2
   peer %actor%
   say If da king hadnt told us not ta, I'd run ya through rite now.
elseif %actor.align% > 0 & %actor.cha% > 13 & %pos% < 5
   say Hrm.  Ya must be one o'da kingz friendz.  Youz best be gittin back ta yer room.
elseif %actor.align% > 75
   mkill %actor%
else
   grumble
   say I don't much like dem big peepel da kingz bin talkin ta.
end
~
#2002
Kobold Wyvern Flight~
0 e 1
growls swears~
if %actor.vnum% == 2010
   say Da wyvern's are loose!  RUN FER YER LIVES!!!
  wait 2s
   flee
   say The WYVERNS ARE LOOSE!!
   curse
   flee
elseif %actor.vnum == 2002
   say Da wyvern's are loose!  RUN FER YER LIVES!!!!
   flee
  wait 2s
   say The WYVERNS ARE LOOSE!!
   curse
   flee
~
#2005
new trigger~
1 h 100
~
wait 3s
oteleport %actor% 2000
~
#2010
Wyvern Growl~
0 bg 50
~
if %random.2% == 1
   snarl
else
   growl
~
#2045
Room 2045 Southern wall writting trigger~
2 c 100
write~
wechoaround %actor% %actor.name% writes something on the southern wall.
wsend %actor% You pick up some blood and write on the southern wall.
wdoor 2045 south description Written in blood on the southern wall, you see, "&R%arg%&w"
~
#2048
Grey Staff Trigger~
1 c 3
read~
if %arg% == runes
   if %actor.class% == mage || cleric || artisan
      osend %actor% On the staff is written the word "&WWennyTiffle&w"
   else
      osend %actor% You don't seem able to understand the runes.
   end
~
#2050
Youlit's speech~
0 b 25
~
say I wish I cood git bak adem sumhow, fer leavin me here.
~
#2051
Addition for Trigger 2050~
0 d 1
who how~
peer %actor%
say Ye look like yer pretty able ta take care of yerself... couldja help m'out abit?
wait 2
say Well.. ya see.. m'probs like dis.  I was sent ere wid a bunch of me mates to bash dese wyverns up, cause da tribe, dere afraid of em.
wait 2s
say But when we got ere, our weapons broke an we were left powerless gainst em beasties!  We waz setup by da king, cause we didnta like dem big peepholes he waz dealin with.
wait 2s
say We didn't like doin dem favours so Bingellytut could get rich, it wasna fair!
wait 2s
say Anyhow, I needz you peepel to just do one itty bitty thing fer me... open dat big iron door and let dem lissards run around in da caves fer a bit.
wait 2s
say It'll drive the tribe nuts, an wid luck, dey'll git da king.
wait 2s
say Do watcha want, I dunna care either way... Big peepholes, dere all da same, ALL DA SAME I TELLZ YA!
~
#2052
Important Mob Bye Bye trigger~
0 g 100
~
if %actor.vnum% > 0
   wait 1s
   say Git out ya little runt, er I'll run ya through!
   mforce %actor% flee
elseif %random.20% > 15
  wait 2s
   emote grunts.
~
#2060
Altar movement trigger~
2 c 4
stand~
if %arg% == on altar
   wechoaround %actor% %actor.name% stands on the altar.
   wsend %actor% You stand on the altar, which begins to vibrate slightly.
  wait 3s
   wechoaround %actor% The altars surface slowly descends, taking %actor.name% with it!
   wsend %actor% You are lowered beneath the altar.
   wteleport %actor% 2061
~
#2067
new trigger~
2 g 100
~
say My trigger commandlist is not complete!
~
#2089
Bone Shrine Trigger~
2 g 85
~
if %actor.vnum% == -1
wait 2
wecho As you enter this room, the many bones on the floor begin to rattle.
wait 2
wecho The bones form small skeletal animals!
wload mob 2032
wload mob 2032
wload mob 2032
~
#2090
Helga's Trigger~
0 g 100
~
if %actor.align% > -100
   say Well met kind Hero!  I am Helga, a warrior from the north.
  wait 2s
   say I was taken prisoner by these foul beasts during a raid on my village.
  wait 2s
   say I seek revenge against them, and shall travel with thee for the time being.
   follow %actor.name%
   detach mob Helga 1
else
   say Vile beasts, what luck that you should come upon me just as I have freed myself from my shackles!  Prepare yourself heathen!
   mkill %actor%
   detach mob Helga 1
~
#2091
Altar of Bone trigger~
1 c 7
impale~
if %arg.name% == corpse
  wait 1s
   osend %actor% You raise the corpse over the altar, and impale it on the spike.
   oechoaround %actor% %actor.name% raises a corpse over the altar, and impales it on the spike.
  wait 2s
   oecho The blood from the corpse begins to drip down the altars sides, slowly.
  wait 3s
   oecho Small depressions in the ground around the altar begin to fill, revealing them as ancient runes.
  wait 4s
   osend %actor% A dark mist rises from the altar, and covers you.
   oechoaround %actor% A dark mist rises from the altar, and washes over %actor.name%
   odamage %actor% -5
   opurge corpse
else
   osend %actor% You don't seem to be able to impale that. . .
~
#2092
Oil trap~
2 g 10
~
if %actor.vnum% == -1
wecho As you enter this room, a small light appears near the ceiling, and begins to fall.
wait 1s
wecho The light appears to be a small torch. . .
wait 1s
wecho The torch touches the water in which you stand, igniting the slick substance that floats on top of it!!
wdamage all %random.10%
~
#2093
Rock Trap~
2 g 25
~
if %actor.vnum% == -1
   if %actor.dex% < %random.18%
      wait 1s
      wechoaround %actor% A large rock falls on %actor.name%!
      wsend %actor% A large rock falls from the ceiling, right onto you!
      wdamage %actor% %random.8%
   end
   else
      wait 1s
      wechoaround %actor% %actor.name% dodges a falling rock!
      wsend %actor% You manage to dodge a falling rock!
   end
else
wechoaround %actor% %actor.name% grins slyly.
end
~
#2094
Dart Trap Trigger~
2 g 40
~
set gib %random.18%
   if %actor.dex% < %gib%
      wait 1s
      wechoaround %actor% A dart flies out of the wall and hits %actor.name%!
      wsend %actor% A dart flies out of the wall and hits you!
      wdamage %actor% %random.5%
   elseif %actor.dex% > %gib%
      wait 1s
      wechoaround %actor% %actor.name% narrowly dodges a dart trap!
      wsend %actor% You narrowly avoid being hit by a dart trap!
~
#2095
Bone Incense trigger~
1 c 2
burn~
if %arg% == incense
   osend %actor% You burn the powdered bone incense.
   oechoaround %actor% %actor.name% burns some powdered incense.
  wait 3s
   oecho You hear the rattling of bones nearby.
  wait 2s
   oecho Suddenly, a skeletal animal appears from the shadows!
   oload mob 2032
   opurge %self%
~
#2096
Reset Rock trigger in 2062~
2 f 100
~
wdoor 2062 down flags 32
~
#2097
Rock in Room 2062 Trigger~
2 c 100
slide~
if %arg% == rock
   wechoaround %actor% %actor.name% slides the large rock into a crevice.
   wsend %actor% You slide the rock into the crevice.
  wait 2s
   wecho You hear a faint 'click.'
   wdoor 2062 down flags 1
  wait 2s
   wecho The rock slowly slides out of the crevice.
elseif %arg% != rock
   wsend %actor% Huh?!?
end
~
#2098
closing ooze~
2 g 100
~
wdoor 2059 north purge
wdoor 2059 north description To the north is a wall coated in ooze.
wdoor 2059 north room -1
~
#2099
waterfall secret~
0 c 100
pull~
if %arg% == crystal || lever
   wdoor 2059 north room 2062
   wdoor 2059 north name Ooze Door
   wechoaround %actor% %actor.name% pulls a small crystal which releases a small 'click.'
   wsend %actor% A quiet 'click' issues forth from the north wall as you pull the small crystal.
elseif %arg% != crystal
   wsend %actor% Huh?!?
end
~
$~
