#1400
Falling Trigger for Room 1412~
2 g 100
~
eval dmg %actor.level%*%random.6%
if %direction% == north
   wechoaround %actor% %actor.name% leaps off the balcony!
   wsend %actor% You leap off the balcony, and begin to fall like a rock!!
   wait 1
   wteleport %actor% 1403
   wechoaround %actor% %actor.name% falls into the room from above, hitting the ground HARD!
   wsend %actor% You hit the ground, HARD.
wdamage %actor% %dmg%
wforce %actor% look
~
$~
